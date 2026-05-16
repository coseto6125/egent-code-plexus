//! Shared atomic write primitives.

use serde::Serialize;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Atomic write: tmp → fsync → rename. The temp path is `<path>.tmp`
/// (appended, not extension-replaced) so any failure mode — `Ctrl+C`,
/// crash, OOM kill — leaves either the previous file intact or a
/// recognizable `.tmp` sibling, never a half-written target.
///
/// `parent_dir` is created if missing. The temp file is `truncate=true,
/// create=true` so a stale `.tmp` from a prior abort is overwritten
/// cleanly.
pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp = tmp_sibling(path);
    {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Atomic JSON write: serialize → [`atomic_write_bytes`]. Caller is
/// responsible for any backup/.bak orchestration outside this call.
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    atomic_write_bytes(path, &bytes)
}

/// Append `.tmp` to the path's last component. Unlike `with_extension`,
/// this preserves the original extension — `graph.bin` → `graph.bin.tmp`
/// (not `graph.tmp`) — so two writers targeting different file types in
/// the same directory cannot collide on the same temp name.
fn tmp_sibling(path: &Path) -> PathBuf {
    let mut buf: OsString = path.as_os_str().to_owned();
    buf.push(".tmp");
    PathBuf::from(buf)
}
