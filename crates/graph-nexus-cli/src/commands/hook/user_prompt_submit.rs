//! UserPromptSubmit handler: surface async reindex outcomes via marker
//! files, then unlink them so each event fires only once. Failure takes
//! priority over success because it is more actionable.

use super::common::{emit_additional_context, gitnexus_dir, HookInput};
use graph_nexus_core::GnxError;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Window we read from the end of `last-rebuild.log` to extract the
/// last few lines. Sized so that even a noisy 3-attempt indexer run
/// (with multi-KB stderr per attempt) fits in one seek+read.
const LOG_TAIL_WINDOW: u64 = 4096;

pub fn handle(input: &HookInput) -> Result<(), GnxError> {
    let gnx_dir = match gitnexus_dir(&input.cwd) {
        Some(d) => d,
        None => return Ok(()),
    };
    let complete = gnx_dir.join(".rebuild-complete");
    let failed = gnx_dir.join(".rebuild-failed");
    let log = gnx_dir.join("last-rebuild.log");

    if failed.exists() {
        let tail = read_log_tail(&log, 3);
        let _ = fs::remove_file(&failed);
        let msg = format!(
            "gnx background reindex FAILED. {} Run `gnx admin index` manually to retry.",
            if tail.is_empty() {
                String::new()
            } else {
                format!("Last log lines: {tail}.")
            }
        );
        emit_additional_context("UserPromptSubmit", msg.trim());
        return Ok(());
    }

    if complete.exists() {
        let stats = read_stats(&gnx_dir);
        let _ = fs::remove_file(&complete);
        let msg = format!("gnx index rebuild complete ({stats}). gnx tools now return fresh data.");
        emit_additional_context("UserPromptSubmit", &msg);
    }
    Ok(())
}

/// Read the last `lines` non-empty lines of `log` by seeking to the
/// end and pulling at most `LOG_TAIL_WINDOW` bytes. Falls back to
/// reading from offset 0 for files smaller than the window. Returns
/// `String::new()` if the file is missing / unreadable — UserPromptSubmit
/// must never block on log access.
fn read_log_tail(log: &Path, lines: usize) -> String {
    let mut f = match fs::File::open(log) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(LOG_TAIL_WINDOW);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::with_capacity(LOG_TAIL_WINDOW as usize);
    if f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut collected: Vec<&str> = text.trim().lines().rev().take(lines).collect();
    collected.reverse();
    collected.join(" | ")
}

fn read_stats(gnx_dir: &Path) -> String {
    let raw = match fs::read_to_string(gnx_dir.join("meta.json")) {
        Ok(s) => s,
        Err(_) => return "?".into(),
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return "?".into(),
    };
    let nodes = v
        .get("node_count")
        .and_then(|x| x.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".into());
    let edges = v
        .get("edge_count")
        .and_then(|x| x.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".into());
    format!("{nodes} symbols, {edges} rels")
}
