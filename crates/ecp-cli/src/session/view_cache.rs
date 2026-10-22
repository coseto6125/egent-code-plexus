//! Cross-process memo of the session's `OverlayView`.
//!
//! Every query process on a dirty tree rebuilt the same view: on guava with
//! four dirty Java files, `OverlayView::build` took 12 ms of a 28 ms query.
//! The session dir keeps the last built view in [`VIEW_CACHE_FILE`] under a
//! key of everything `build` reads, so a later process with the same inputs
//! decodes it instead.
//!
//! File layout: key (u64 LE) · xxh3 of the view bytes (u64 LE) · view bytes
//! ([`OverlayView::to_cache_bytes`]). The checksum catches a flipped byte
//! that rkyv validation alone would accept as different but valid data.
//! Every I/O failure is a miss: the caller builds the view as before.

use ecp_core::registry::BUILDER_FINGERPRINT;
use ecp_core::session::{OverlayView, VIEW_CACHE_FORMAT};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use xxhash_rust::xxh3::{xxh3_64, Xxh3};

pub const VIEW_CACHE_FILE: &str = "view_cache.bin";

const HEADER_LEN: usize = 16;

/// The cache key of a view built over the graph file `graph` = (path,
/// length, mtime), as stat'ed from the handle the engine mapped, with
/// `graph_nodes` nodes, from inputs whose hash is `inputs_hash`
/// (`accepted_fragments`). The running binary is part of the key: its build
/// sha alone misses a dev rebuild of a dirty checkout, so its length and
/// mtime join too. `None` when the binary cannot be stat'ed or a stamp
/// predates the epoch; the caller then skips the cache.
pub fn key(graph: (&Path, u64, SystemTime), graph_nodes: usize, inputs_hash: u64) -> Option<u64> {
    let exe = std::env::current_exe().ok()?;
    let exe_meta = fs::metadata(&exe).ok()?;
    let mut hasher = Xxh3::new();
    hasher.update(&VIEW_CACHE_FORMAT.to_le_bytes());
    hash_part(&mut hasher, BUILDER_FINGERPRINT.as_bytes());
    hash_part(&mut hasher, env!("ECP_GIT_SHA").as_bytes());
    hash_stamp(&mut hasher, &exe, exe_meta.len(), exe_meta.modified().ok()?)?;
    let (graph_path, graph_len, graph_mtime) = graph;
    hash_stamp(&mut hasher, graph_path, graph_len, graph_mtime)?;
    hasher.update(&(graph_nodes as u64).to_le_bytes());
    hasher.update(&inputs_hash.to_le_bytes());
    Some(hasher.digest())
}

/// Length-prefixed, so two different part sequences never concatenate to
/// the same stream.
pub(crate) fn hash_part(hasher: &mut Xxh3, part: &[u8]) {
    hasher.update(&(part.len() as u64).to_le_bytes());
    hasher.update(part);
}

fn hash_stamp(hasher: &mut Xxh3, path: &Path, len: u64, mtime: SystemTime) -> Option<()> {
    let mtime_ns = mtime.duration_since(UNIX_EPOCH).ok()?.as_nanos();
    hash_part(hasher, path.as_os_str().as_encoded_bytes());
    hasher.update(&len.to_le_bytes());
    hasher.update(&mtime_ns.to_le_bytes());
    Some(())
}

/// The cached view in `session_dir` when it was stored under `key`, its
/// bytes are intact, and it indexes a graph of `graph_nodes` nodes, else
/// `None`. A file under another key costs one header read, not the body.
pub fn read(session_dir: &Path, key: u64, graph_nodes: usize) -> Option<OverlayView> {
    let mut file = File::open(session_dir.join(VIEW_CACHE_FILE)).ok()?;
    let mut header = [0u8; HEADER_LEN];
    file.read_exact(&mut header).ok()?;
    let (stored_key, checksum) = header.split_at(8);
    if u64::from_le_bytes(stored_key.try_into().ok()?) != key {
        return None;
    }
    let mut body = Vec::new();
    file.read_to_end(&mut body).ok()?;
    if u64::from_le_bytes(checksum.try_into().ok()?) != xxh3_64(&body) {
        return None;
    }
    OverlayView::from_cache_bytes(&body, graph_nodes)
}

/// Store `view` under `key` in `session_dir`. Best effort: a missing or
/// read-only dir leaves no cache, and the session dir is never created
/// here. Concurrent writers each publish a whole file by rename, so a
/// reader sees one writer's file or the other's, never a mix.
pub fn write(session_dir: &Path, key: u64, view: &OverlayView) {
    let body = view.to_cache_bytes();
    if body.is_empty() {
        return;
    }
    let mut bytes = Vec::with_capacity(HEADER_LEN + body.len());
    bytes.extend_from_slice(&key.to_le_bytes());
    bytes.extend_from_slice(&xxh3_64(&body).to_le_bytes());
    bytes.extend_from_slice(&body);
    // Engines in one process share the pid; tests build many at once.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = session_dir.join(format!(
        "{VIEW_CACHE_FILE}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    if fs::write(&tmp, &bytes)
        .and_then(|()| fs::rename(&tmp, session_dir.join(VIEW_CACHE_FILE)))
        .is_err()
    {
        let _ = fs::remove_file(&tmp);
    }
}
