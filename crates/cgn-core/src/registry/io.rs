//! Shared atomic write primitives.

use serde::Serialize;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomic write: tmp → fsync → rename. Temp path is per-writer unique
/// (`<path>.<pid>.<counter>.tmp`) so two concurrent writers — common in
/// the multi-agent setup where pre-tool-use hook, MCP background indexer,
/// and CLI all race to refresh the same `dirty_files.json` / metadata
/// JSON — cannot truncate the same tmp inode and produce a final file
/// with one writer's prefix glued onto another's tail (the corruption
/// shape that broke search_batch tests with "trailing characters at line
/// 15741 column 2" — two valid JSON documents concatenated). The unique
/// suffix also avoids ENOENT races where two writers both call rename on
/// the same `<path>.tmp` and the second one fails because the first
/// already consumed the source.
///
/// `parent_dir` is created if missing. Each tmp file is `truncate=true,
/// create=true`. Stale `.tmp` siblings from aborted writes are still
/// recognizable via the `.tmp` suffix and can be swept by cleanup tools.
pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
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

/// Append `<pid>.<counter>.tmp` to the path's last component. Unlike
/// `with_extension`, this preserves the original extension — `graph.bin`
/// → `graph.bin.<pid>.<n>.tmp` — so two writers targeting different file
/// types in the same directory cannot collide on the same temp name, AND
/// two writers targeting the SAME file from concurrent processes get
/// disjoint inodes (Round 81 fix: the previous shared-`.tmp` design let
/// concurrent writers truncate the same inode and produce stacked-JSON
/// corruption).
fn unique_tmp_sibling(path: &Path) -> PathBuf {
    let mut buf: OsString = path.as_os_str().to_owned();
    buf.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    PathBuf::from(buf)
}
