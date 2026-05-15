//! PreToolUse handler: extract a search pattern from Grep / Glob / Bash
//! invocations, run an in-process gnx search, and inject the top-K
//! hits into the conversation as `additionalContext`. Capped at 5 hits
//! or ~2 KB serialized to keep the token cost bounded.

use super::common::{emit_additional_context, gitnexus_dir, strip_shell_quotes, HookInput};
use crate::commands::search::{compute_hits, Hit, SearchArgs, SearchMode};
use crate::engine::Engine;
use graph_nexus_core::GnxError;
use std::sync::OnceLock;

const MAX_HITS: usize = 5;
const MAX_BYTES: usize = 2048;
const HITS_HEADER: &str = "gnx graph hits:\n";

/// Glob-stem extractor. Compiled once per process — PreToolUse fires
/// on every Grep / Glob / Bash so amortising the regex build matters.
fn glob_stem_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"[*/]([a-zA-Z][a-zA-Z0-9_-]{2,})").unwrap())
}

pub fn handle(input: &HookInput) -> Result<(), GnxError> {
    let pattern = match extract_pattern(&input.tool_name, &input.tool_input) {
        Some(p) if p.len() >= 3 => p,
        _ => return Ok(()),
    };
    let gnx_dir = match gitnexus_dir(&input.cwd) {
        Some(d) => d,
        None => return Ok(()),
    };
    let graph_path = gnx_dir.join("graph.bin");
    let engine = match Engine::load(&graph_path) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    let args = SearchArgs {
        pattern,
        mode: SearchMode::Auto,
        kind: None,
        repo: None,
        format: None,
    };
    let hits = match compute_hits(args, &engine) {
        Ok(h) => h,
        Err(_) => return Ok(()),
    };
    if hits.is_empty() {
        return Ok(());
    }
    let lines = format_hits(&hits);
    if lines.is_empty() {
        return Ok(());
    }
    emit_additional_context("PreToolUse", &lines);
    Ok(())
}

fn format_hits(hits: &[Hit]) -> String {
    let mut out = String::from(HITS_HEADER);
    for h in hits.iter().take(MAX_HITS) {
        let line = format!(
            "  [{}] {}:{} {} (callers:{}) score:{:.3}\n",
            h.kind, h.file, h.line, h.name, h.caller_count, h.score
        );
        if out.len() + line.len() > MAX_BYTES {
            break;
        }
        out.push_str(&line);
    }
    // If no row was appended, the buffer still equals the header — caller
    // treats an empty return as "no hits".
    if out.len() == HITS_HEADER.len() {
        String::new()
    } else {
        out
    }
}

fn extract_pattern(tool: &str, tool_input: &serde_json::Value) -> Option<String> {
    match tool {
        "Grep" => tool_input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        "Glob" => {
            let raw = tool_input
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            glob_stem_re().captures(raw).map(|c| c[1].to_string())
        }
        "Bash" => {
            let cmd = tool_input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let stripped = strip_shell_quotes(cmd);
            extract_from_shell(&stripped)
        }
        _ => None,
    }
}

/// Single-pass scan: locate `rg` / `grep`, then walk subsequent tokens
/// to find the first ≥3-char non-flag positional. Returns `None` if
/// `rg` / `grep` is absent or every token after it is a flag / flag
/// value / too short.
fn extract_from_shell(cmd: &str) -> Option<String> {
    let flags_with_values = [
        "-e",
        "-f",
        "-m",
        "-A",
        "-B",
        "-C",
        "-g",
        "--glob",
        "-t",
        "--type",
        "--include",
        "--exclude",
    ];
    let mut found_cmd = false;
    let mut skip_next = false;
    for token in cmd.split_whitespace() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if !found_cmd {
            if token == "rg" || token == "grep" {
                found_cmd = true;
            }
            continue;
        }
        if token.starts_with('-') {
            if flags_with_values.contains(&token) {
                skip_next = true;
            }
            continue;
        }
        let cleaned: String = token.chars().filter(|c| *c != '"' && *c != '\'').collect();
        if cleaned.len() >= 3 {
            return Some(cleaned);
        }
    }
    None
}
