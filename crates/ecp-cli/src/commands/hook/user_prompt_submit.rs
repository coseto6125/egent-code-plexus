//! UserPromptSubmit handler: surface async reindex outcomes via marker
//! files, then unlink them so each event fires only once. Failure takes
//! priority over success because it is more actionable.

use super::common::{ecp_state_dir, hook_specific_output, lookup_index_dir, HookInput};
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
    if let Some(payload) = render_output(&sections, drain_update_notice()) {
        println!("{payload}");
    }
    Ok(())
}

/// The update notice tells the reader to run `ecp update`, an action the model
/// must not take. It goes to `systemMessage` (shown to the human), never to
/// `additionalContext` (read by the model). One object: Claude Code parses a
/// single JSON value from stdout.
fn render_output(sections: &[String], update: Option<String>) -> Option<serde_json::Value> {
    let mut out = serde_json::Map::new();
    if !sections.is_empty() {
        out.extend(
            hook_specific_output(
                "UserPromptSubmit",
                &[("additionalContext", &sections.join("\n\n"))],
            )
            .as_object()
            .cloned()
            .expect("the envelope is an object"),
        );
    }
    if let Some(update) = update {
        out.insert("systemMessage".into(), update.into());
    }
    (!out.is_empty()).then_some(serde_json::Value::Object(out))
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
    // a repo-controlled path, so the path is confined first. Resolved, not
    // read: this function reads at most `LOG_TAIL_WINDOW` bytes, and validating
    // by reading the whole file would hand a repository the allocation size.
    let log = match super::common::resolve_within_repo(log, repo_root) {
        Some(p) => p,
        None => return String::new(),
    };
    let mut f = match fs::File::open(&log) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_output_update_notice_goes_to_system_message_not_model_context() {
        let notice = "ecp 9.9.9 available. Run `ecp update`.".to_string();
        let sections = vec!["ecp index rebuild complete.".to_string()];
        let v = render_output(&sections, Some(notice.clone())).unwrap();
        assert_eq!(v["systemMessage"], notice);
        let ctx = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(
            !ctx.contains("ecp update"),
            "model context carries the notice: {ctx}"
        );
        assert_eq!(ctx, "ecp index rebuild complete.");
    }

    #[test]
    fn test_render_output_update_only_has_no_additional_context() {
        let v = render_output(&[], Some("new version".into())).unwrap();
        assert!(v.get("hookSpecificOutput").is_none());
        assert_eq!(v["systemMessage"], "new version");
    }

    #[test]
    fn test_render_output_nothing_to_say_emits_nothing() {
        assert!(render_output(&[], None).is_none());
    }
}
