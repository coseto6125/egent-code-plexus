//! Log rotation + retention constants for peer-sync logs.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const MSG_LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;
pub const MSG_LOG_KEEP_ROTATED: usize = 7;
pub const WATCHER_LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;
pub const WATCHER_LOG_KEEP_ROTATED: usize = 3;
pub const SESSION_STALE_DAYS: i64 = 30;
pub const ARCHIVE_PURGE_DAYS: i64 = 90;
pub const ROTATE_CHECK_EVERY_N_EVENTS: u32 = 100;

/// Rotate `log` if it exceeds `threshold_bytes`. Chains existing rotated files
/// (`log.1` → `log.2`, …, dropping `log.{keep+1}` if present). Truncates `log`
/// after rotation. Returns whether rotation happened.
pub fn rotate_if_needed(log: &Path, threshold_bytes: u64, keep: usize) -> io::Result<bool> {
    let meta = match fs::metadata(log) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if meta.len() < threshold_bytes {
        return Ok(false);
    }
    let dir = log.parent().unwrap_or_else(|| Path::new("."));
    let stem = log.file_name().and_then(|s| s.to_str()).unwrap_or("log");
    let path_n = |n: usize| -> PathBuf { dir.join(format!("{stem}.{n}")) };

    if path_n(keep).exists() {
        fs::remove_file(path_n(keep))?;
    }
    for n in (1..keep).rev() {
        let from = path_n(n);
        if from.exists() {
            fs::rename(&from, path_n(n + 1))?;
        }
    }
    fs::rename(log, path_n(1))?;
    fs::write(log, b"")?;
    Ok(true)
}
