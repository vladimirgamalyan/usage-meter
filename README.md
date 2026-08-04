# Usage Meter

A tiny native Windows app that shows how much of your AI agent rate limits is
used up, for three providers at once:

| Provider | What it shows |
|---|---|
| **Claude Code** (Anthropic) | 5-hour window and 7-day window |
| **Codex** (OpenAI/ChatGPT) | both rate-limit windows plus granted rate-limit reset credits |
| **Antigravity** (Google) | 5-hour and weekly Gemini quota |

Each value is rendered as a slim rounded fill bar with a text like
`42% · 2h11m` — the consumed share and a live countdown to the window reset.

```
Usage Meter                        ─ ×
Settings
──────────────────────────────────────
  Claude Code
    5h  ████░░░░░░░░░░░  42% · 2h11m
    7d  ██░░░░░░░░░░░░░  18% · 4d2h
  Codex
    5h  ░░░░░░░░░░░░░░░  --
    7d  ███████████░░░░  73% · 4d3h
     ↺                   8d17h
```

## Key properties

- **No backend of its own.** The app reads local credentials already saved by
  the providers' CLI tools and talks to their endpoints directly.
- **Never writes credentials.** Refreshing an expired token is delegated to
  the corresponding CLI (`claude` / `codex`), started as a hidden child
  process.
- **No telemetry, no auto-update**, no network activity beyond polling usage.
- The only persistent file is the window settings.

## Features

- Slim pill-style fill bars, light/dark theme following the system, colored
  per provider.
- Live countdowns that tick without extra network requests; expired Codex
  reset credits disappear on their own.
- Providers are toggled from the menu; the window resizes to fit.
- Auth errors pause polling and watch the credential stores (file, WSL,
  Credential Manager); polling resumes automatically after you sign in again.
- Transient network errors retry with exponential backoff.
- Per-Monitor V2 DPI awareness, position persistence, single-instance guard.
- Proxy support (`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY`) with a CONNECT tunnel:
  TLS stays end-to-end, the proxy never sees bearer tokens.

## Credentials it reads

| Provider | Source |
|---|---|
| Claude Code | `%USERPROFILE%\.claude\.credentials.json`, falling back to WSL distributions (`~/.claude/.credentials.json`) |
| Codex | `%CODEX_HOME%\auth.json` or `%USERPROFILE%\.codex\auth.json` |
| Antigravity | Windows Credential Manager, generic credential `gemini:antigravity` |

## Build

Requires Rust (edition 2021) on Windows with the MSVC toolchain.

```
cargo build --release
```

The binary is `target\release\usage-meter.exe` (~450 KB). TLS uses the system
SChannel via `native-tls`; no root certificates are bundled.

## Usage

Run `usage-meter.exe`. The **Settings** menu (also available as a right-click
context menu) provides:

- **Refresh** — poll immediately.
- **Update Frequency** — 1 minute to 1 hour (default 15 minutes).
- **Reset Credit Frequency** — how often the Codex reset-credit list is
  re-fetched (default 6 hours); shown only when Codex is enabled.
- **Models** — which providers to show (at least one is always enabled).

Settings are stored in `%APPDATA%\UsageMeter\settings.json`.

### Diagnostics

Start with `--diagnose` to write a log to `%TEMP%\usage-meter.log`
(recreated on every start). Without the flag logging is fully disabled.
