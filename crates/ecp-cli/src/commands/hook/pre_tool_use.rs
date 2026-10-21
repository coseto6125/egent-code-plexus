//! PreToolUse handler: extract a search pattern from Grep / Glob / Bash
//! invocations, run an in-process `ecp find --mode bm25`, and inject
//! the top-K hits into the conversation as `additionalContext`. Capped
//! at 5 hits or ~2 KB serialized to keep the token cost bounded.

use super::common::{emit_additional_context, lookup_index_dir, HookInput};
use crate::commands::find::{compute_hits, FindArgs, FindMode, Hit};
use crate::engine::Engine;
use ecp_core::EcpError;
use std::path::{Path, PathBuf};

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
    if let Some(flow) = super::edit_flow::context(input, false) {
        sections.push(flow);
    }
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
    let cd_dir = (input.tool_name == "Bash")
        .then(|| bash_command(&input.tool_input).and_then(|c| cd_target(c, &input.cwd)))
        .flatten();
    let index_dir = lookup_index_dir(cd_dir.as_deref().unwrap_or(&input.cwd))?;
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

fn bash_command(tool_input: &serde_json::Value) -> Option<&str> {
    tool_input.get("command").and_then(|v| v.as_str())
}

/// The control operator that ends a shell segment.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Sep {
    /// `;` or a newline.
    Seq,
    And,
    Or,
    Pipe,
    Background,
}

/// Split a shell command at unquoted `;`, `&&`, `||`, `|`, `&` and newlines,
/// each segment paired with the operator that ends it (`None` for the last).
/// Every separator is ASCII, so the byte offsets stay on char boundaries for
/// CJK paths.
fn shell_segments(cmd: &str) -> Vec<(&str, Option<Sep>)> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quote: Option<char> = None;
    let mut chars = cmd.char_indices().peekable();
    let mut prev = ' ';
    while let Some((i, c)) = chars.next() {
        let after_redirect = matches!(prev, '>' | '<');
        prev = c;
        let sep = match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => {
                quote = None;
                continue;
            }
            (Some('"'), '\\') | (None, '\\') => {
                chars.next();
                continue;
            }
            (Some(_), _) => continue,
            (None, '\'' | '"') => {
                quote = Some(c);
                continue;
            }
            (None, ';' | '\n') => Sep::Seq,
            // `2>&1`, `<&0`, `>|file` and `&>file` are redirects, not operators.
            (None, '&' | '|') if after_redirect => continue,
            (None, '&') if chars.peek().is_some_and(|&(_, n)| n == '>') => continue,
            (None, '&' | '|') => match (c, chars.next_if(|&(_, n)| n == c)) {
                ('&', Some(_)) => Sep::And,
                ('&', None) => Sep::Background,
                (_, Some(_)) => Sep::Or,
                (_, None) => Sep::Pipe,
            },
            _ => continue,
        };
        segments.push((&cmd[start..i], Some(sep)));
        start = chars.peek().map_or(cmd.len(), |&(next, _)| next);
    }
    segments.push((&cmd[start..], None));
    segments
}

/// Directory a Bash search runs in when a literal `cd <dir>` precedes the
/// `rg` / `grep` segment: sessions working across repos keep the envelope cwd
/// on another repo, so resolving from it finds no graph. The last valid `cd`
/// wins; `cd -`, `$VAR` / `$(..)` / backtick paths, `~user`, a missing
/// directory and a bare `cd` are ignored (no expansion, no shell spawn). The
/// `contains("cd")` check keeps a command without `cd` at one substring scan.
fn cd_target(cmd: &str, cwd: &str) -> Option<String> {
    cd_target_with_home(cmd, cwd, std::env::var_os("HOME").as_deref().map(Path::new))
}

fn cd_target_with_home(cmd: &str, cwd: &str, home: Option<&Path>) -> Option<String> {
    if !cmd.contains("cd") {
        return None;
    }
    let mut current: Option<PathBuf> = None;
    let mut before = Some(Sep::Seq);
    for (segment, after) in shell_segments(cmd) {
        let words = shell_words(segment);
        // `extract_from_shell` takes the first `rg` / `grep` word anywhere
        // (`rtk proxy rg`, `git grep`), so the scan stops at the same word.
        if words.iter().any(|w| w == "rg" || w == "grep") {
            break;
        }
        // A cd moves the search only when it surely runs in the search's
        // shell: not in a pipeline or a background job, not after `||`, and
        // after `&&` only inside an `&&` chain that the search belongs to.
        let runs_here = match before {
            Some(Sep::Seq) => matches!(after, None | Some(Sep::Seq | Sep::And)),
            Some(Sep::And) => after == Some(Sep::And),
            _ => false,
        };
        if runs_here && words.len() == 2 && words[0] == "cd" {
            // A quoted `'~/x'` names a directory called `~`.
            let tilde = segment
                .trim_start()
                .strip_prefix("cd")
                .is_some_and(|rest| rest.trim_start().starts_with('~'));
            let base = current.as_deref().unwrap_or(Path::new(cwd));
            if let Some(dir) = resolve_cd_arg(&words[1], base, home, tilde) {
                current = Some(dir);
            }
        }
        before = after;
    }
    current?.into_os_string().into_string().ok()
}

fn resolve_cd_arg(arg: &str, base: &Path, home: Option<&Path>, tilde: bool) -> Option<PathBuf> {
    if arg.is_empty() || arg == "-" || arg.contains(['$', '`']) {
        return None;
    }
    let path = match arg.strip_prefix('~').filter(|_| tilde) {
        Some("") => home?.to_path_buf(),
        Some(rest) if rest.starts_with('/') => home?.join(rest.trim_start_matches('/')),
        Some(_) => return None,
        None if Path::new(arg).is_absolute() => PathBuf::from(arg),
        None if base.is_absolute() => base.join(arg),
        None => return None,
    };
    let canonical = path.canonicalize().ok()?;
    canonical.is_dir().then_some(canonical)
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
            let cmd = bash_command(tool_input).unwrap_or("");
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
    use super::{cd_target_with_home, extract_from_shell, search_terms, shell_segments, Sep};
    use std::path::Path;

    fn canon(p: &Path) -> String {
        p.canonicalize().unwrap().to_str().unwrap().to_string()
    }

    fn target(cmd: &str, cwd: &Path) -> Option<String> {
        cd_target_with_home(cmd, cwd.to_str().unwrap(), None)
    }

    #[test]
    fn test_cd_target_no_cd_returns_none() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(target("grep -rn Foo app", t.path()), None);
        assert_eq!(target("", t.path()), None);
        assert_eq!(target("   ", t.path()), None);
    }

    #[test]
    fn test_cd_target_quoted_cjk_path_with_spaces_resolves() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("業主 原始碼").join("outsourcing-backend-CN");
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();
        let other = tempfile::tempdir().unwrap();
        for cmd in [
            format!("cd \"{d}\" && grep -rn CacheKeyQueue app"),
            format!("cd '{d}' && grep -rn CacheKeyQueue app"),
            format!("cd {} && rg Foo", d.replace(' ', "\\ ")),
        ] {
            assert_eq!(target(&cmd, other.path()), Some(canon(&dir)), "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_relative_to_envelope_cwd_resolves() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("sub")).unwrap();
        assert_eq!(
            target("cd sub && grep -rn Foo .", t.path()),
            Some(canon(&t.path().join("sub")))
        );
    }

    #[test]
    fn test_cd_target_home_tilde_expands_via_home_only() {
        let h = tempfile::tempdir().unwrap();
        std::fs::create_dir(h.path().join("x")).unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let go = |cmd: &str, home| cd_target_with_home(cmd, cwd.path().to_str().unwrap(), home);
        assert_eq!(
            go("cd ~ && grep Foo", Some(h.path())),
            Some(canon(h.path()))
        );
        assert_eq!(
            go("cd ~/x && grep Foo", Some(h.path())),
            Some(canon(&h.path().join("x")))
        );
        assert_eq!(go("cd ~/x && grep Foo", None), None);
        assert_eq!(go("cd ~bob && grep Foo", Some(h.path())), None);
    }

    #[test]
    fn test_cd_target_dash_bare_var_and_subshell_ignored() {
        let t = tempfile::tempdir().unwrap();
        for cmd in [
            "cd - && grep Foo .",
            "cd && grep Foo .",
            "cd $HOME && grep Foo .",
            "cd \"$(pwd)/x\" && grep Foo .",
            "cd `pwd` && grep Foo .",
        ] {
            assert_eq!(target(cmd, t.path()), None, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_nonexistent_dir_falls_back() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(target("cd /no/such/dir_zz && grep Foo .", t.path()), None);
    }

    #[test]
    fn test_cd_target_last_cd_wins_and_chains_relative() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("a/b")).unwrap();
        std::fs::create_dir(t.path().join("c")).unwrap();
        assert_eq!(
            target("cd a && cd b && grep Foo .", t.path()),
            Some(canon(&t.path().join("a/b")))
        );
        assert_eq!(
            target("cd a && cd ../c && grep Foo .", t.path()),
            Some(canon(&t.path().join("c")))
        );
        // A cd after the search segment does not move the search.
        assert_eq!(
            target("cd a && grep Foo . ; cd ../c", t.path()),
            Some(canon(&t.path().join("a")))
        );
    }

    #[test]
    fn test_cd_target_semicolon_and_or_separators_resolve() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        let want = Some(canon(&t.path().join("x")));
        assert_eq!(target("cd x; grep Foo .", t.path()), want);
        assert_eq!(target("cd x\ngrep Foo .", t.path()), want);
    }

    #[test]
    fn test_cd_target_cd_that_may_not_run_in_search_shell_ignored() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        for cmd in [
            "true || cd x; rg Foo",
            "false || cd x && rg Foo",
            "cd x | rg Foo",
            "cd x & rg Foo",
            "make && cd x; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), None, "{cmd}");
        }
        assert_eq!(
            target("make && cd x && rg Foo", t.path()),
            Some(canon(&t.path().join("x")))
        );
    }

    #[test]
    fn test_cd_target_search_word_anywhere_stops_scan() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        for cmd in [
            "rtk proxy rg Foo; cd x",
            "git grep Foo; cd x",
            "find . | xargs grep Foo; cd x",
        ] {
            assert_eq!(target(cmd, t.path()), None, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_quoted_tilde_is_a_relative_dir() {
        let h = tempfile::tempdir().unwrap();
        std::fs::create_dir(h.path().join("repo")).unwrap();
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cwd.path().join("~/repo")).unwrap();
        let go = |cmd: &str| cd_target_with_home(cmd, cwd.path().to_str().unwrap(), Some(h.path()));
        let literal = Some(canon(&cwd.path().join("~/repo")));
        assert_eq!(go("cd '~/repo'; rg Foo"), literal);
        assert_eq!(go("cd \"~/repo\" && rg Foo"), literal);
        assert_eq!(
            go("cd ~/repo && rg Foo"),
            Some(canon(&h.path().join("repo")))
        );
    }

    #[test]
    fn test_cd_target_cd_inside_quotes_is_not_a_segment() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().to_str().unwrap();
        let cmd = format!("echo \"cd {d}\" && grep -rn Foo app");
        assert_eq!(target(&cmd, t.path()), None);
        let cmd = format!("echo 'a; cd {d}' && grep -rn Foo app");
        assert_eq!(target(&cmd, t.path()), None);
    }

    #[test]
    fn test_shell_segments_splits_only_unquoted_separators() {
        let segs: Vec<(&str, Option<Sep>)> = shell_segments("cd 'a;b' && grep \"x|y\" . | head")
            .into_iter()
            .map(|(s, sep)| (s.trim(), sep))
            .collect();
        assert_eq!(
            segs,
            [
                ("cd 'a;b'", Some(Sep::And)),
                ("grep \"x|y\" .", Some(Sep::Pipe)),
                ("head", None)
            ]
        );
        assert_eq!(
            shell_segments("make 2>&1 <&0 &>log >|f; x"),
            [("make 2>&1 <&0 &>log >|f", Some(Sep::Seq)), (" x", None)]
        );
    }

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
