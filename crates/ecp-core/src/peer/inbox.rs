//! Inbox transport — append-only JSON lines, drain-and-truncate semantics.

use crate::peer::concern::ConcernKind;
use crate::session::overlay::SymbolRef;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InboxEntry {
    DirtyEvent {
        ts: String,
        peer_session: String,
        peer_pid: u32,
        /// Team-visible name of the peer session, when its meta carries one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer_name: Option<String>,
        kind: ConcernKindSer,
        symbol: SymbolRef,
        reason: String,
        peer_delta: Option<String>,
        your_overlap_range: Option<(u32, u32)>,
    },
    Message {
        ts: String,
        msg_id: String,
        from: String,
        /// Team-visible name of the sender, when its meta carries one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_name: Option<String>,
        to: Option<String>,
        reply_to: Option<String>,
        body: String,
    },
}

/// Serde-friendly mirror of [`ConcernKind`] — `ConcernKind` itself lacks serde
/// derives to keep `peer::concern` dependency-free.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ConcernKindSer {
    Hard,
    Soft,
}

impl From<ConcernKind> for ConcernKindSer {
    fn from(k: ConcernKind) -> Self {
        match k {
            ConcernKind::Hard => Self::Hard,
            ConcernKind::Soft => Self::Soft,
        }
    }
}

/// Path of the generation sidecar for an inbox file.
///
/// The sidecar stores a monotonically increasing `u32` generation counter as
/// 4 raw little-endian bytes.  `append_entry` bumps the counter whenever it
/// writes to a zero-length file, giving `drain` a reliable truncation signal
/// even when the file is rewritten to the same byte length within a single
/// clock tick (filesystem mtime granularity on some kernels is 1 s).
fn gen_path(inbox: &Path) -> std::path::PathBuf {
    let mut p = inbox.as_os_str().to_owned();
    p.push(".gen");
    std::path::PathBuf::from(p)
}

fn read_gen(inbox: &Path) -> io::Result<u32> {
    let gp = gen_path(inbox);
    match std::fs::File::open(&gp) {
        Ok(mut f) => {
            let mut buf = [0u8; 4];
            f.read_exact(&mut buf)?;
            Ok(u32::from_le_bytes(buf))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

fn bump_gen(inbox: &Path) -> io::Result<u32> {
    let gp = gen_path(inbox);
    let next = read_gen(inbox)?.wrapping_add(1);
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&gp)?;
    f.write_all(&next.to_le_bytes())?;
    Ok(next)
}

/// Append one entry as a newline-terminated JSON line.
///
/// Uses `O_APPEND` so each `write_all` is atomic at the OS level provided the
/// serialised line is shorter than `PIPE_BUF` (4 096 bytes on Linux).
/// Bumps the generation sidecar when appending to a zero-length file so that
/// `drain` can detect truncation even on coarse-mtime filesystems.
pub fn append_entry(path: &Path, entry: &InboxEntry) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_vec(entry).map_err(io::Error::other)?;
    line.push(b'\n');
    debug_assert!(
        line.len() < 4096,
        "inbox entry must fit in PIPE_BUF for atomic append"
    );
    let _guard = InboxLock::acquire(path)?;
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    // Bump generation when appending to an empty file (fresh create or truncation).
    if f.metadata()?.len() == 0 {
        bump_gen(path)?;
    }
    f.write_all(&line)?;
    Ok(())
}

/// Exclusive lock shared by every inbox writer, reader and rotator.
///
/// O_APPEND alone orders concurrent writes, but says nothing about a reader
/// snapshotting the length mid-write, or a rotator renaming the file between a
/// writer opening it and writing its line. Both lost messages. One lock over
/// all three operations is the cheapest protocol that closes them: an append is
/// a single sub-PIPE_BUF line and a drain runs once per hook, so contention is
/// not a factor.
///
/// The guard is held for its Drop side effect only; the field is never read.
pub struct InboxLock(#[allow(dead_code)] Option<crate::registry::FileLock>);

impl InboxLock {
    /// Best-effort, for writers. A lock we cannot take must not stop a peer
    /// message from being written: an unsynchronised append is worse than
    /// nothing only in theory, a dropped message is worse in practice.
    pub fn acquire(inbox: &Path) -> io::Result<Self> {
        let lock_path = inbox.with_extension("jsonl.lock");
        Ok(Self(
            crate::registry::FileLock::acquire_exclusive(&lock_path).ok(),
        ))
    }

    /// Required, for anything that REMOVES entries. Proceeding unlocked there
    /// would erase a concurrent append; failing instead costs one turn of
    /// latency and the entries are still in the file.
    pub fn require(inbox: &Path) -> io::Result<Self> {
        let lock_path = inbox.with_extension("jsonl.lock");
        Ok(Self(Some(crate::registry::FileLock::acquire_exclusive(
            &lock_path,
        )?)))
    }
}

/// Read every entry, hand them to `deliver`, and remove exactly the ones it
/// reports as delivered — all inside one lock.
///
/// The lock has to span the read AND the rewrite. Releasing it in between is
/// what let a sender's append land in the window and be erased by the caller's
/// truncate, which is the bug this whole path exists to prevent. `deliver`
/// returns the indices it actually represented; everything else is written
/// back, so an entry the payload could not fit is still there next time.
///
/// Lines that do not parse (a writer killed mid-write leaves a partial tail)
/// are dropped rather than carried forward, so they cannot fuse with the next
/// append into one corrupt line.
pub fn deliver_and_consume<T>(
    path: &Path,
    deliver: impl FnOnce(&[InboxEntry]) -> (T, std::collections::HashSet<usize>),
) -> io::Result<Option<T>> {
    let _guard = InboxLock::require(path)?;
    let content = match std::fs::read(path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut entries = Vec::new();
    let mut raw = Vec::new();
    for line in content.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(line);
        match serde_json::from_str::<InboxEntry>(text.trim_end()) {
            Ok(entry) => {
                entries.push(entry);
                raw.push(text.into_owned());
            }
            Err(e) => tracing::warn!(error = %e, "dropping unparseable inbox line"),
        }
    }
    if entries.is_empty() {
        // Nothing deliverable, but the file may still hold an unparseable tail
        // from a killed writer. Clearing it here is what stops the next append
        // fusing onto it into one corrupt line.
        if !content.is_empty() {
            let _ = write_lines(path, &[]);
        }
        return Ok(None);
    }
    let (value, consumed) = deliver(&entries);
    let kept: Vec<&String> = raw
        .iter()
        .enumerate()
        .filter(|(i, _)| !consumed.contains(i))
        .map(|(_, l)| l)
        .collect();
    write_lines(path, &kept)?;
    Ok(Some(value))
}

/// Replace the inbox with `lines`, atomically. An in-place `write` that is
/// interrupted leaves the file truncated, which would lose every entry the
/// payload deliberately held back — the opposite of the point.
fn write_lines(path: &Path, lines: &[&String]) -> io::Result<()> {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, out.as_bytes())?;
    crate::registry::replace_file(&tmp, path)?;
    // Best-effort: the generation sidecar only guards watermark readers, and
    // there are none left on this path. Losing the bump must not discard a
    // payload the agent is about to be shown.
    if let Err(e) = bump_gen(path) {
        tracing::warn!(error = %e, "inbox generation bump failed");
    }
    Ok(())
}

// Watermark encoding: upper 32 bits = generation counter, lower 32 bits =
// byte offset (max 4 GiB per inbox file — sufficient for JSONL inboxes).
const OFFSET_MASK: u64 = u32::MAX as u64;
const GEN_SHIFT: u64 = 32;

fn pack_watermark(offset: u64, gen: u32) -> u64 {
    ((gen as u64) << GEN_SHIFT) | (offset & OFFSET_MASK)
}

fn unpack_watermark(w: u64) -> (u64, u32) {
    (w & OFFSET_MASK, (w >> GEN_SHIFT) as u32)
}

/// Atomically truncate inbox and bump the generation sidecar so the next
/// `drain` detects truncation correctly.
///
/// Call this instead of `fs::write(path, "")` in hook drain paths.  The
/// generation bump ensures that a `drain` holding an old watermark will see
/// the gen mismatch and reset to byte 0, rather than silently missing entries
/// that were appended between our drain-read and our truncate.
pub fn truncate_inbox(path: &Path) -> io::Result<()> {
    // Same lock as `append_entry` and `drain`: without it a sender's line can
    // land between this write and a reader's length snapshot and be erased.
    let _guard = InboxLock::acquire(path)?;
    std::fs::write(path, "")?;
    bump_gen(path)?;
    Ok(())
}

/// Read entries after `start_offset`, returning `(entries, new_watermark)`.
///
/// The watermark is an opaque `u64` — pass the value returned by a previous
/// `drain` call back as `start_offset`.  Passing `0` reads from the beginning.
///
/// Detects external truncation via a generation sidecar (`.gen` file bumped by
/// `append_entry` on every write to an empty file).  Resets to byte 0 when
/// truncation is detected.  Corrupt / non-JSON lines are skipped with a
/// warning.
pub fn drain(path: &Path, start_offset: u64) -> io::Result<(Vec<InboxEntry>, u64)> {
    // Lock BEFORE opening: taking it after leaves a window in which the file we
    // hold and the generation we read can come from different inodes, and the
    // watermark we return then mixes one file's offset with another's counter.
    let _guard = InboxLock::acquire(path)?;
    let mut f = match OpenOptions::new().read(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(e),
    };
    let len = f.metadata()?.len();
    let cur_gen = read_gen(path)?;

    let (prev_byte_off, prev_gen) = unpack_watermark(start_offset);

    // Reset to 0 if file shrank below watermark OR generation changed
    // (truncation detected even when file regrew to same size).
    let truncated = prev_byte_off > len || (prev_byte_off > 0 && cur_gen != prev_gen);
    let from = if truncated { 0 } else { prev_byte_off };

    f.seek(SeekFrom::Start(from))?;
    // Read only as far as the length we snapshotted. A sender appending while
    // this streams would otherwise be delivered here and still fall after the
    // returned watermark, so the next drain would deliver it a second time —
    // and a non-empty append does not bump `.gen`, so nothing else catches it.
    let mut reader = BufReader::new((&mut f).take(len.saturating_sub(from)));
    let mut out = Vec::new();
    // The watermark advances to the last COMPLETE line, not to the snapshot
    // length. A writer killed mid-line leaves a partial tail; consuming up to
    // `len` would step over it, and the bytes that later complete that line
    // would then be unreachable — taking the next message down with them.
    let mut complete = from;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        if !buf.ends_with(b"\n") {
            break; // partial tail — leave it for the next drain
        }
        complete += n as u64;
        let line = String::from_utf8_lossy(&buf);
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<InboxEntry>(line.trim_end()) {
            Ok(entry) => out.push(entry),
            Err(e) => {
                tracing::warn!(error = %e, "skipping corrupt inbox line");
            }
        }
    }
    Ok((out, pack_watermark(complete, cur_gen)))
}
