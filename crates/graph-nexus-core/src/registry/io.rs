//! Shared atomic write primitives.

use serde::Serialize;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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

/// Same as [`atomic_write_bytes`] but skips `fsync` for write throughput.
/// Use only for content-addressable / regeneratable data (parse cache,
/// derived artifacts) where a torn write on crash is acceptable — the
/// next read either deletes the corrupt blob and reparses, or never
/// finds it because the rename didn't reach disk. The tmp+rename still
/// provides atomicity within a single process lifetime; each writer uses
/// a unique temp sibling so duplicate content-hash writes cannot trample
/// each other's temp file. We just don't pay the per-file fsync cost
/// (~2ms each on typical SSDs, which dominates cold-index time when
/// cache puts are O(10⁴)).
pub fn atomic_write_bytes_no_fsync(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp = unique_tmp_sibling(path);
    {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        // Intentional: no sync_all. See doc comment.
    }
    fs::rename(&tmp, path)?;
    Ok(())
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

fn unique_tmp_sibling(path: &Path) -> PathBuf {
    let mut buf: OsString = path.as_os_str().to_owned();
    buf.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    PathBuf::from(buf)
}
