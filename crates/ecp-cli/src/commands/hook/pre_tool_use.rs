//! PreToolUse handler: extract a search pattern from Grep / Glob / Bash
//! invocations, run an in-process `ecp find --mode bm25`, and inject
//! the top-K hits into the conversation as `additionalContext`. Capped
//! at 5 hits or ~2 KB serialized to keep the token cost bounded.

use super::common::{emit_additional_context, lookup_index_dir, HookInput};
use crate::commands::find::{compute_hits, FindArgs, FindMode, Hit};
use crate::engine::Engine;
use ecp_core::EcpError;
use std::sync::OnceLock;

const MAX_HITS: usize = 5;
const MAX_BYTES: usize = 2048;
const HITS_HEADER: &str = "ecp graph hits:\n";

/// Glob-stem extractor. Compiled once per process — PreToolUse fires
/// on every Grep / Glob / Bash so amortising the regex build matters.
fn glob_stem_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"[*/]([a-zA-Z][a-zA-Z0-9_-]{2,})").unwrap())
}

pub fn handle(input: &HookInput) -> Result<(), EcpError> {
    // Both signals (graph hits + peer drain) must be merged into a single
    // additionalContext payload — Claude Code parses one JSON object on
    // stdout, so two separate println!s would drop the second silently.
    let mut sections: Vec<String> = Vec::new();
    if let Some(hits) = compute_search_hits(input) {
        sections.push(hits);
    }
    if let Some(peer) = super::common::drain_and_render_peer_payload() {
        sections.push(peer);
    }
    if !sections.is_empty() {
        emit_additional_context("PreToolUse", &sections.join("\n\n"));
    }
    Ok(())
}

fn compute_search_hits(input: &HookInput) -> Option<String> {
    let pattern = match extract_pattern(&input.tool_name, &input.tool_input) {
        Some(p) if p.len() >= 3 => p,
        _ => return None,
    };
    let index_dir = lookup_index_dir(&input.cwd)?;
    let graph_path = index_dir.join("graph.bin");
    let engine = Engine::load(&graph_path).ok()?;
    let args = FindArgs {
        pattern: Some(pattern),
        mode: FindMode::Bm25,
        fuzzy: false,
        all: false,
        include_tests: false,
        kind: None,
        repo: None,
        format: None,
        batch: false,
    };
    let hits = compute_hits(args, &engine).ok()?;
    if hits.is_empty() {
        return None;
    }
    let lines = format_hits(&hits);
    (!lines.is_empty()).then_some(lines)
}

/// Render hits as a legacy-style multi-line block. Each symbol gets a
/// header `name (file:line) [kind]` followed by optional `Called by:`
/// and `Calls:` lines drawn from the in-process 1-hop CSR expansion in
/// `compute_hits`. Empty caller / callee lists are skipped to keep the
/// per-hit footprint tight; the LLM reads the absence as "no callers
/// found within 1 hop" rather than asking ecp for a deeper trace.
pub fn format_hits(hits: &[Hit]) -> String {
    let mut out = String::from(HITS_HEADER);
    for h in hits.iter().take(MAX_HITS) {
        let mut block = format!("  {} ({}:{}) [{}]\n", h.name, h.file, h.line, h.kind);
        if !h.callers.is_empty() {
            block.push_str(&format!("    Called by: {}\n", h.callers.join(", ")));
        }
        if !h.callees.is_empty() {
            block.push_str(&format!("    Calls: {}\n", h.callees.join(", ")));
        }
        if out.len() + block.len() > MAX_BYTES {
            break;
        }
        out.push_str(&block);
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
            // Do NOT pass through `strip_shell_quotes` here — it deletes the
            // entire quoted block, which is precisely where the grep / rg
            // pattern lives (e.g. `rg "summary_blind_spots"`). The downstream
            // token-level `cleaned` filter in `extract_from_shell` peels the
            // surviving quote characters off each token. `strip_shell_quotes`
            // is still the right tool for `post_tool_use` git-mutation
            // detection (where ignoring `echo "git commit"` is a feature).
            extract_from_shell(cmd)
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

#[cfg(test)]
mod tests {
    use super::extract_from_shell;

    #[test]
    fn grep_double_quoted_pattern_extracted() {
        // Regression: `strip_shell_quotes` used to delete the entire quoted
        // block, leaving the hook to pick up the next non-flag token (often
        // a pipe-side `head` / `tail`) and surface unrelated graph noise.
        let cmd = r#"git show abc:foo.rs | grep -nE "summary_blind_spots" | head -20"#;
        assert_eq!(
            extract_from_shell(cmd),
            Some("summary_blind_spots".to_string())
        );
    }

    #[test]
    fn grep_single_quoted_pattern_extracted() {
        let cmd = "rg -n 'validateUser' src/";
        assert_eq!(extract_from_shell(cmd), Some("validateUser".to_string()));
    }

    #[test]
    fn grep_regex_metachars_preserved() {
        let cmd = r#"grep -E "(compute_single|score|bm25)" file.rs"#;
        assert_eq!(
            extract_from_shell(cmd),
            Some("(compute_single|score|bm25)".to_string())
        );
    }

    #[test]
    fn no_grep_returns_none() {
        assert_eq!(extract_from_shell("cat foo.txt | head -20"), None);
    }
}
