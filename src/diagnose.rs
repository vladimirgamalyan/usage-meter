//! Optional diagnostics log, enabled with `--diagnose`.
//!
//! Without the flag every `log` call is a no-op (a single `OnceLock` check).

use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static LOG: OnceLock<Mutex<File>> = OnceLock::new();

const LOG_FILE_NAME: &str = "usage-meter.log";

/// Enable logging: recreate the log file in `%TEMP%`.
pub fn init() {
    let path = std::env::temp_dir().join(LOG_FILE_NAME);
    if let Ok(file) = File::create(path) {
        let _ = LOG.set(Mutex::new(file));
    }
}

/// Append a line to the log with immediate flush. No-op unless `init` ran.
pub fn log(message: &str) {
    let Some(lock) = LOG.get() else {
        return;
    };
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut file = lock.lock().unwrap_or_else(|e| e.into_inner());
    let _ = writeln!(file, "[{unix}] {message}");
    let _ = file.flush();
}
