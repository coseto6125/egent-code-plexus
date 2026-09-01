//! PreToolUse handler: extract a search pattern from Grep / Glob / Bash
//! invocations, run an in-process `ecp find --mode bm25`, and inject
//! the top-K hits into the conversation as `additionalContext`. Capped
//! at 5 hits or ~2 KB serialized to keep the token cost bounded.

use super::common::{emit_additional_context, lookup_index_dir, HookInput};
use crate::commands::find::{compute_hits, FindArgs, FindMode, Hit};
use crate::engine::Engine;
use ecp_core::EcpError;

const MAX_HITS: usize = 5;
const MAX_BYTES: usize = 2048;
const HITS_HEADER: &str = "ecp graph hits:\n";

/// Glob-stem extractor. Compiled once per process — PreToolUse fires
/// on every Grep / Glob / Bash so amortising the regex build matters.
fn glob_stem_re() -> &'static regex::Regex {
    regex::regex!(r"[*/]([a-zA-Z][a-zA-Z0-9_-]{2,})")
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
    let pattern = search_terms(&extract_pattern(&input.tool_name, &input.tool_input)?)?;
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
        file: None,
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

/// The identifier-shaped words of a grep / rg pattern, space-joined for the
/// BM25 query. Regex syntax carries no symbol signal: `^\[t\]` used to reach
/// tantivy as the token `t` and surface five unrelated `t` functions, and
/// `\bfetch\s*\(` as `bfetch`. Escapes (`\X`) are dropped whole, every other
/// non-word character splits, and a word survives only with three or more
/// characters and at least one letter. `None` skips the graph load entirely.
fn search_terms(pattern: &str) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut flush = |current: &mut String| {
        if current.chars().count() >= 3 && current.chars().any(char::is_alphabetic) {
            terms.push(std::mem::take(current));
        } else {
            current.clear();
        }
    };
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            flush(&mut current);
            chars.next();
        } else if c.is_alphanumeric() || c == '_' {
            current.push(c);
        } else {
            flush(&mut current);
        }
    }
    flush(&mut current);
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// Split a shell command into words the way the shell hands them to the
/// program: quotes group, and are removed; a backslash outside single quotes
/// escapes the next character. A quoted pattern such as
/// `grep "pub struct HookInput"` is one word, not three.
fn shell_words(cmd: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                current.extend(chars.by_ref().take_while(|&q| q != '\''));
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        // Inside double quotes a backslash escapes only `$`,
                        // `` ` ``, `"`, `\\` and newline; before anything else
                        // it stays literal, which is what keeps a regex like
                        // `"\bfetch\s*\("` intact.
                        '\\' => match chars.next() {
                            Some('\n') => {}
                            Some(escaped @ ('$' | '`' | '"' | '\\')) => current.push(escaped),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => current.push('\\'),
                        },
                        _ => current.push(q),
                    }
                }
            }
            '\\' => {
                in_word = true;
                current.extend(chars.next());
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            _ => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

/// Single-pass scan: locate `rg` / `grep`, then walk subsequent words
/// to find the first ≥3-char non-flag positional. Returns `None` if
/// `rg` / `grep` is absent or every word after it is a flag / flag
/// value / too short.
fn extract_from_shell(cmd: &str) -> Option<String> {
    let flags_with_values = [
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
    // `-e PATTERN` names the pattern explicitly for both grep and rg; the
    // word after it is the answer, not a flag value to step over.
    let mut next_is_pattern = false;
    for word in shell_words(cmd) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if !found_cmd {
            if word == "rg" || word == "grep" {
                found_cmd = true;
            }
            continue;
        }
        if word.starts_with('-') && !next_is_pattern {
            if word == "-e" || word == "--regexp" {
                next_is_pattern = true;
            } else if flags_with_values.contains(&word.as_str()) {
                skip_next = true;
            }
            continue;
        }
        if word.len() >= 3 {
            return Some(word);
        }
        next_is_pattern = false;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{extract_from_shell, search_terms};

    #[test]
    fn grep_quoted_multiword_pattern_kept_whole() {
        // Whitespace splitting handed the hook `"pub` (cleaned to `pub`) and
        // the graph answered with every `pub_*` symbol in the repo.
        let cmd = r#"grep -n "pub struct HookInput" -A12 crates/x.rs | head"#;
        assert_eq!(
            extract_from_shell(cmd),
            Some("pub struct HookInput".to_string())
        );
        let cmd = "rg -n 'fn compute hits' src/";
        assert_eq!(extract_from_shell(cmd), Some("fn compute hits".to_string()));
    }

    #[test]
    fn dash_e_names_the_pattern_instead_of_hiding_it() {
        assert_eq!(
            extract_from_shell("rg -e validateUser src/"),
            Some("validateUser".to_string())
        );
        assert_eq!(
            extract_from_shell("grep -rn --regexp validateUser src/"),
            Some("validateUser".to_string())
        );
        // A pattern that itself starts with a dash is still the pattern.
        assert_eq!(
            extract_from_shell("rg -e -foo_bar src/"),
            Some("-foo_bar".to_string())
        );
    }

    #[test]
    fn grep_double_quoted_regex_keeps_its_backslashes() {
        // POSIX: inside "..." a backslash before anything other than
        // $ ` " \\ or newline is literal, so the regex reaches search_terms
        // unchanged and reduces to `fetch`, not `bfetchs`.
        let cmd = r#"grep -rn "\bfetch\s*\(" src/"#;
        let raw = extract_from_shell(cmd);
        assert_eq!(raw.as_deref(), Some(r"\bfetch\s*\("));
        assert_eq!(
            search_terms(raw.as_deref().unwrap()).as_deref(),
            Some("fetch")
        );
        let cmd = r#"grep "a\$b\\c" f"#;
        assert_eq!(extract_from_shell(cmd), Some(r"a$b\c".to_string()));
    }

    #[test]
    fn grep_escaped_quote_inside_pattern_survives() {
        let cmd = r#"grep -rn "say \"hi\" now" ."#;
        assert_eq!(extract_from_shell(cmd), Some(r#"say "hi" now"#.to_string()));
    }

    #[test]
    fn search_terms_splits_regex_into_identifiers() {
        assert_eq!(
            search_terms("(compute_single|score|bm25)").as_deref(),
            Some("compute_single score bm25")
        );
        assert_eq!(search_terms("HookInput").as_deref(), Some("HookInput"));
        assert_eq!(
            search_terms("pub struct HookInput").as_deref(),
            Some("pub struct HookInput")
        );
    }

    #[test]
    fn search_terms_drops_escapes_and_short_words() {
        assert_eq!(search_terms(r"\bfetch\s*\(").as_deref(), Some("fetch"));
        assert_eq!(search_terms("fn .*uid").as_deref(), Some("uid"));
        assert_eq!(search_terms(r"^\[t\]"), None);
        assert_eq!(search_terms("12345"), None);
        assert_eq!(search_terms("ab"), None);
    }

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
