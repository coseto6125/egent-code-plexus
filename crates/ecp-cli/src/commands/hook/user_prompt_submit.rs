//! UserPromptSubmit handler: surface async reindex outcomes via marker
//! files, then unlink them so each event fires only once. Failure takes
//! priority over success because it is more actionable.

use super::common::{ecp_state_dir, emit_additional_context, lookup_index_dir, HookInput};
use ecp_core::EcpError;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Window we read from the end of `last-rebuild.log` to extract the
/// last few lines. Sized so that even a noisy 3-attempt indexer run
/// (with multi-KB stderr per attempt) fits in one seek+read.
const LOG_TAIL_WINDOW: u64 = 4096;

pub fn handle(input: &HookInput) -> Result<(), EcpError> {
    // All signals (rebuild marker + peer drain) merge into one
    // additionalContext payload — Claude Code parses one JSON object on
    // stdout, so two println!s would drop the second silently.
    let mut sections: Vec<String> = Vec::new();

    if let Some(state_dir) = ecp_state_dir(&input.cwd) {
        let complete = state_dir.join(".rebuild-complete");
        let failed = state_dir.join(".rebuild-failed");
        let log = state_dir.join("last-rebuild.log");

        if failed.exists() {
            let tail = read_log_tail(&log, 3, Path::new(&input.cwd));
            let _ = fs::remove_file(&failed);
            let msg = format!(
                "ecp background reindex FAILED. {} Run `ecp admin index` manually to retry.",
                if tail.is_empty() {
                    String::new()
                } else {
                    format!("Last log lines: {tail}.")
                }
            );
            sections.push(msg.trim().to_string());
        } else if complete.exists() {
            let stats = lookup_index_dir(&input.cwd)
                .map(|d| read_stats(&d))
                .unwrap_or_else(|| "?".into());
            let _ = fs::remove_file(&complete);
            sections.push(format!(
                "ecp index rebuild complete ({stats}). ecp tools now return fresh data."
            ));
        }
    }

    if let Some(peer) = super::common::drain_and_render_peer_payload() {
        sections.push(peer);
    }
    if let Some(update) = drain_update_notice() {
        sections.push(update);
    }
    if !sections.is_empty() {
        emit_additional_context("UserPromptSubmit", &sections.join("\n\n"));
    }
    Ok(())
}

/// Read and consume the global `<home_ecp>/.update-available` marker written by
/// the background `admin check-update` probe. Returns its text (the upgrade
/// notice) once, then unlinks it so the notice fires a single time. Missing
/// marker → `None`. Lives at home_ecp (not the per-repo state dir) because the
/// probe is repo-independent.
fn drain_update_notice() -> Option<String> {
    let marker = ecp_core::registry::resolve_home_ecp().join(".update-available");
    let body = fs::read_to_string(&marker).ok()?;
    let _ = fs::remove_file(&marker);
    let trimmed = body.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Read the last `lines` non-empty lines of `log` by seeking to the
/// end and pulling at most `LOG_TAIL_WINDOW` bytes. Falls back to
/// reading from offset 0 for files smaller than the window. Returns
/// `String::new()` if the file is missing / unreadable — UserPromptSubmit
/// must never block on log access.
fn read_log_tail(log: &Path, lines: usize, repo_root: &Path) -> String {
    // The tail is quoted into the notice the model reads, and the log lives at
    // a repo-controlled path, so it goes through the confined read first.
    if super::common::read_within_repo(log, repo_root).is_none() {
        return String::new();
    }
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

fn read_stats(index_dir: &Path) -> String {
    let raw = match fs::read_to_string(index_dir.join("meta.json")) {
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
