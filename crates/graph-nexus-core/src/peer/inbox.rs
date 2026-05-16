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
        to: Option<String>,
        reply_to: Option<String>,
        body: String,
    },
}

/// Serde-friendly mirror of [`ConcernKind`] — `ConcernKind` itself lacks serde
/// derives to keep `peer::concern` dependency-free.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    // Bump generation when appending to an empty file (fresh create or truncation).
    if f.metadata()?.len() == 0 {
        bump_gen(path)?;
    }
    f.write_all(&line)?;
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
    let reader = BufReader::new(&mut f);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<InboxEntry>(&line) {
            Ok(entry) => out.push(entry),
            Err(e) => {
                tracing::warn!(error = %e, "skipping corrupt inbox line");
            }
        }
    }
    Ok((out, pack_watermark(len, cur_gen)))
}
