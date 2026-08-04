//! Minimal HTTP/1.1 client with TLS (SChannel via native-tls) and proxy support.
//!
//! One request-response pair per TCP connection (`Connection: close`).
//! Redirects are followed for GET only (up to 3). Bodies are limited to 8 MB.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_REDIRECTS: u32 = 3;
const DEFAULT_USER_AGENT: &str = "usage-meter";

pub struct Response {
    pub status: u16,
    /// Header names are lowercased on read; lookup is case-insensitive.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(n, _)| *n == lower)
            .map(|(_, v)| v.as_str())
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Soft JSON parse: a malformed body yields `None`, never a panic.
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

pub fn get(url: &str, headers: &[(&str, &str)]) -> Result<Response, String> {
    request("GET", url, headers, None)
}

pub fn post(url: &str, headers: &[(&str, &str)], body: &[u8]) -> Result<Response, String> {
    request("POST", url, headers, Some(body))
}

struct Url {
    scheme: String,
    host: String,
    port: u16,
    /// Path including the query string.
    path: String,
}

impl Url {
    fn is_default_port(&self) -> bool {
        (self.scheme == "http" && self.port == 80) || (self.scheme == "https" && self.port == 443)
    }

    fn host_header(&self) -> String {
        if self.is_default_port() {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn parse_url(url: &str) -> Result<Url, String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("no scheme in URL: {url}"))?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(format!("unsupported scheme: {scheme}"));
    }
    let (authority, path) = match rest.find(['/', '?']) {
        Some(idx) if rest.as_bytes()[idx] == b'/' => (&rest[..idx], rest[idx..].to_string()),
        Some(idx) => (&rest[..idx], format!("/{}", &rest[idx..])),
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() {
        return Err(format!("empty host in URL: {url}"));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            (h.to_string(), p.parse::<u16>().map_err(|e| e.to_string())?)
        }
        _ => (
            authority.to_string(),
            if scheme == "https" { 443 } else { 80 },
        ),
    };
    Ok(Url {
        scheme,
        host: host.to_ascii_lowercase(),
        port,
        path,
    })
}

// --- Proxy configuration -----------------------------------------------------

struct Proxy {
    host: String,
    port: u16,
    auth: Option<(String, String)>,
}

/// First non-empty of the given environment variables, checked in both upper
/// and lower case.
fn env_first(names: &[&str]) -> Option<String> {
    for name in names {
        for candidate in [name.to_ascii_uppercase(), name.to_ascii_lowercase()] {
            if let Ok(value) = std::env::var(&candidate) {
                if !value.trim().is_empty() {
                    return Some(value.trim().to_string());
                }
            }
        }
    }
    None
}

fn no_proxy_matches(host: &str) -> bool {
    let Some(list) = env_first(&["NO_PROXY"]) else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    for entry in list.split(',') {
        let entry = entry.trim().to_ascii_lowercase();
        if entry.is_empty() {
            continue;
        }
        if entry == "*" {
            return true;
        }
        let suffix = entry.trim_start_matches('.');
        if host == suffix || host.ends_with(&format!(".{suffix}")) {
            return true;
        }
    }
    false
}

fn parse_proxy(raw: &str) -> Option<Proxy> {
    let (scheme, rest) = match raw.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => (String::new(), raw),
    };
    let rest = rest.trim_end_matches('/');
    let (auth, host_port) = match rest.rsplit_once('@') {
        Some((cred, hp)) => {
            let (user, pass) = cred.split_once(':').unwrap_or((cred, ""));
            (Some((user.to_string(), pass.to_string())), hp)
        }
        None => (None, rest),
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            (h.to_string(), p.parse::<u16>().ok()?)
        }
        _ => (
            host_port.to_string(),
            if scheme == "https" { 443 } else { 80 },
        ),
    };
    if host.is_empty() {
        return None;
    }
    Some(Proxy { host, port, auth })
}

fn proxy_for(url: &Url) -> Option<Proxy> {
    if no_proxy_matches(&url.host) {
        return None;
    }
    let raw = env_first(&["ALL_PROXY"]).or_else(|| {
        if url.scheme == "https" {
            env_first(&["HTTPS_PROXY"])
        } else {
            env_first(&["HTTP_PROXY"])
        }
    })?;
    parse_proxy(&raw)
}

// --- Base64 (manual, to avoid a dependency) ----------------------------------

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(BASE64_ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(BASE64_ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// --- Connection --------------------------------------------------------------

enum Stream {
    Plain(TcpStream),
    Tls(Box<native_tls::TlsStream<TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

fn connect_tcp(host: &str, port: u16) -> Result<TcpStream, String> {
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("DNS resolution failed for {host}: {e}"))?
        .collect();
    let mut last_err = format!("no addresses for {host}");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, TIMEOUT) {
            Ok(stream) => {
                stream
                    .set_read_timeout(Some(TIMEOUT))
                    .map_err(|e| e.to_string())?;
                stream
                    .set_write_timeout(Some(TIMEOUT))
                    .map_err(|e| e.to_string())?;
                return Ok(stream);
            }
            Err(e) => last_err = format!("connect to {addr} failed: {e}"),
        }
    }
    Err(last_err)
}

fn tls_handshake(tcp: TcpStream, host: &str) -> Result<Stream, String> {
    let connector = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    let tls = connector
        .connect(host, tcp)
        .map_err(|e| format!("TLS handshake with {host} failed: {e}"))?;
    Ok(Stream::Tls(Box::new(tls)))
}

/// Read the CONNECT response byte by byte up to the blank line so that no TLS
/// bytes are consumed from the socket.
fn read_connect_response(tcp: &mut TcpStream) -> Result<u16, String> {
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if buf.len() > 64 * 1024 {
            return Err("oversized CONNECT response".to_string());
        }
        match tcp.read(&mut byte) {
            Ok(0) => return Err("proxy closed connection during CONNECT".to_string()),
            Ok(_) => buf.push(byte[0]),
            Err(e) => return Err(format!("CONNECT read failed: {e}")),
        }
    }
    let line = String::from_utf8_lossy(&buf);
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| "malformed CONNECT status line".to_string())?;
    Ok(status)
}

/// Open a stream to the target, tunneling through a proxy when configured.
/// The returned flag is true when the request line must use the absolute form
/// (plain HTTP through a proxy).
fn open_stream(url: &Url, proxy: &Option<Proxy>) -> Result<(Stream, bool), String> {
    match proxy {
        None => {
            let tcp = connect_tcp(&url.host, url.port)?;
            if url.scheme == "https" {
                Ok((tls_handshake(tcp, &url.host)?, false))
            } else {
                Ok((Stream::Plain(tcp), false))
            }
        }
        Some(p) => {
            let mut tcp = connect_tcp(&p.host, p.port)?;
            if url.scheme == "https" {
                // CONNECT tunnel: TLS stays end-to-end, the proxy never sees
                // the bearer token.
                let mut req = format!(
                    "CONNECT {0}:{1} HTTP/1.1\r\nHost: {0}:{1}\r\n",
                    url.host, url.port
                );
                if let Some((user, pass)) = &p.auth {
                    let cred = base64_encode(format!("{user}:{pass}").as_bytes());
                    req.push_str(&format!("Proxy-Authorization: Basic {cred}\r\n"));
                }
                req.push_str("\r\n");
                tcp.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
                tcp.flush().map_err(|e| e.to_string())?;
                let status = read_connect_response(&mut tcp)?;
                if !(200..300).contains(&status) {
                    return Err(format!("proxy CONNECT failed with status {status}"));
                }
                Ok((tls_handshake(tcp, &url.host)?, false))
            } else {
                Ok((Stream::Plain(tcp), true))
            }
        }
    }
}

// --- Request/response --------------------------------------------------------

fn has_header(headers: &[(&str, &str)], name: &str) -> bool {
    headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
}

fn request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<Response, String> {
    let mut current = url.to_string();
    let mut redirects = 0u32;
    loop {
        let parsed = parse_url(&current)?;
        let proxy = proxy_for(&parsed);
        let (mut stream, absolute_form) = open_stream(&parsed, &proxy)?;

        let target = if absolute_form {
            format!(
                "http://{}{}",
                parsed.host_header(),
                parsed.path
            )
        } else {
            parsed.path.clone()
        };
        let mut req = format!("{method} {target} HTTP/1.1\r\n");
        req.push_str(&format!("Host: {}\r\n", parsed.host_header()));
        if absolute_form {
            if let Some(p) = &proxy {
                if let Some((user, pass)) = &p.auth {
                    let cred = base64_encode(format!("{user}:{pass}").as_bytes());
                    req.push_str(&format!("Proxy-Authorization: Basic {cred}\r\n"));
                }
            }
        }
        for (name, value) in headers {
            req.push_str(&format!("{name}: {value}\r\n"));
        }
        if !has_header(headers, "Accept") {
            req.push_str("Accept: application/json\r\n");
        }
        // The client cannot decompress: identity is mandatory.
        if !has_header(headers, "Accept-Encoding") {
            req.push_str("Accept-Encoding: identity\r\n");
        }
        if !has_header(headers, "User-Agent") {
            req.push_str(&format!("User-Agent: {DEFAULT_USER_AGENT}\r\n"));
        }
        req.push_str("Connection: close\r\n");
        // POST always carries Content-Length, even with an empty body.
        if body.is_some() || method == "POST" {
            let len = body.map_or(0, <[u8]>::len);
            req.push_str(&format!("Content-Length: {len}\r\n"));
        }
        req.push_str("\r\n");

        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("request write failed: {e}"))?;
        if let Some(b) = body {
            stream
                .write_all(b)
                .map_err(|e| format!("body write failed: {e}"))?;
        }
        stream.flush().map_err(|e| e.to_string())?;

        let response = read_response(&mut stream, method)?;

        let is_redirect = matches!(response.status, 301 | 302 | 303 | 307 | 308);
        if is_redirect && method == "GET" && redirects < MAX_REDIRECTS {
            if let Some(location) = response.header("location") {
                current = resolve_location(&parsed, location);
                redirects += 1;
                continue;
            }
        }
        return Ok(response);
    }
}

fn resolve_location(base: &Url, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        return location.to_string();
    }
    let origin = if base.is_default_port() {
        format!("{}://{}", base.scheme, base.host)
    } else {
        format!("{}://{}:{}", base.scheme, base.host, base.port)
    };
    if location.starts_with('/') {
        return format!("{origin}{location}");
    }
    // Relative to the current path: strip the query, then cut at the last '/'.
    let path = base.path.split('?').next().unwrap_or("/");
    let dir = match path.rfind('/') {
        Some(idx) => &path[..=idx],
        None => "/",
    };
    format!("{origin}{dir}{location}")
}

fn read_response(stream: &mut Stream, method: &str) -> Result<Response, String> {
    let mut reader = std::io::BufReader::new(stream);
    let status_line = read_line(&mut reader)?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("malformed status line: {status_line}"))?;

    let mut headers = Vec::new();
    loop {
        let line = read_line(&mut reader)?;
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }

    let header = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    };

    let body = if method == "HEAD" || status == 204 || status == 304 {
        Vec::new()
    } else if header("transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false)
    {
        read_chunked(&mut reader)?
    } else if let Some(len) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        if len > MAX_BODY {
            return Err(format!("response body too large: {len}"));
        }
        let mut buf = vec![0u8; len];
        reader
            .read_exact(&mut buf)
            .map_err(|e| format!("body read failed: {e}"))?;
        buf
    } else {
        let mut buf = Vec::new();
        reader
            .by_ref()
            .take(MAX_BODY as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(|e| format!("body read failed: {e}"))?;
        if buf.len() > MAX_BODY {
            return Err("response body too large".to_string());
        }
        buf
    };

    Ok(Response {
        status,
        headers,
        body,
    })
}

/// Read a CRLF-terminated line (the terminator is stripped).
fn read_line<R: Read>(reader: &mut R) -> Result<String, String> {
    let mut line = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    loop {
        if line.len() > 64 * 1024 {
            return Err("oversized header line".to_string());
        }
        match reader.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
            }
            Err(e) => return Err(format!("read failed: {e}")),
        }
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

fn read_chunked<R: Read>(reader: &mut R) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let size_line = read_line(reader)?;
        // Chunk extensions after ';' are ignored.
        let size_str = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| format!("malformed chunk size: {size_line}"))?;
        if size == 0 {
            // Trailers: read until the blank line.
            loop {
                if read_line(reader)?.is_empty() {
                    break;
                }
            }
            break;
        }
        if body.len() + size > MAX_BODY {
            return Err("chunked body too large".to_string());
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader
            .read_exact(&mut body[start..])
            .map_err(|e| format!("chunk read failed: {e}"))?;
        // Trailing CRLF after chunk data.
        let mut crlf = [0u8; 2];
        reader
            .read_exact(&mut crlf)
            .map_err(|e| format!("chunk terminator read failed: {e}"))?;
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_url_forms() {
        let u = parse_url("https://api.anthropic.com/api/oauth/usage").unwrap();
        assert_eq!(u.scheme, "https");
        assert_eq!(u.host, "api.anthropic.com");
        assert_eq!(u.port, 443);
        assert_eq!(u.path, "/api/oauth/usage");
        assert!(u.is_default_port());

        let u = parse_url("http://localhost:8080").unwrap();
        assert_eq!(u.port, 8080);
        assert_eq!(u.path, "/");
        assert_eq!(u.host_header(), "localhost:8080");

        let u = parse_url("https://x.test/a/b?q=1").unwrap();
        assert_eq!(u.path, "/a/b?q=1");
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn parses_proxy_forms() {
        let p = parse_proxy("http://user:pw@proxy.local:3128").unwrap();
        assert_eq!(p.host, "proxy.local");
        assert_eq!(p.port, 3128);
        assert_eq!(p.auth, Some(("user".to_string(), "pw".to_string())));

        let p = parse_proxy("proxy:8888").unwrap();
        assert_eq!(p.host, "proxy");
        assert_eq!(p.port, 8888);
        assert!(p.auth.is_none());

        let p = parse_proxy("http://proxy.local").unwrap();
        assert_eq!(p.port, 80);
    }

    #[test]
    fn resolves_redirect_locations() {
        let base = parse_url("https://a.test/x/y?q=1").unwrap();
        assert_eq!(
            resolve_location(&base, "https://b.test/z"),
            "https://b.test/z"
        );
        assert_eq!(resolve_location(&base, "/root"), "https://a.test/root");
        assert_eq!(resolve_location(&base, "rel"), "https://a.test/x/rel");
    }

    #[test]
    fn reads_chunked_bodies_with_extensions_and_trailers() {
        let raw = b"4;ext=1\r\nWiki\r\n5\r\npedia\r\n0\r\nTrailer: x\r\n\r\n";
        let mut cursor = std::io::Cursor::new(raw.to_vec());
        let body = read_chunked(&mut cursor).unwrap();
        assert_eq!(body, b"Wikipedia");
    }
}
