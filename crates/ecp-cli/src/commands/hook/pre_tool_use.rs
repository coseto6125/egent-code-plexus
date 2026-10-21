//! PreToolUse handler: extract a search pattern from Grep / Glob / Bash
//! invocations, run an in-process `ecp find --mode bm25`, and inject
//! the top-K hits into the conversation as `additionalContext`. Capped
//! at 5 hits or ~2 KB serialized to keep the token cost bounded.

use super::common::{emit_additional_context, lookup_index_dir, HookInput};
use crate::commands::find::{compute_hits, FindArgs, FindMode, Hit};
use crate::engine::Engine;
use ecp_core::EcpError;
use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

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
    let cd = match bash_command(&input.tool_input).filter(|_| input.tool_name == "Bash") {
        Some(cmd) => cd_target(cmd, &input.cwd),
        None => CdTarget::Unchanged,
    };
    let search_dir = match &cd {
        CdTarget::Unchanged => input.cwd.as_str(),
        CdTarget::Dir(dir) => dir.as_str(),
        // Hits from a repo the search may not run in would mislead.
        CdTarget::Unknown => return None,
    };
    let index_dir = lookup_index_dir(search_dir)?;
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
    if lines.is_empty() {
        return None;
    }
    // Hits from the repo a leading `cd` names: the agent's own cwd is
    // another repo, and its paths are relative to this one.
    Some(match &cd {
        CdTarget::Dir(dir) => {
            lines.replacen(HITS_HEADER, &format!("ecp graph hits in {dir}:\n"), 1)
        }
        _ => lines,
    })
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
/// A segment stops before an unquoted `#` that starts a word: the comment's
/// text runs to the newline and holds no operator, quote or group. A
/// `${...}` expansion with no brace, quote or escape inside is one opaque
/// run.
/// `None` when a quote or `${` is left open, since bash rejects the whole
/// command, and for a heredoc, whose body the split does not model. Every
/// separator is ASCII, so the byte offsets stay on char boundaries for CJK
/// paths.
fn shell_segments(cmd: &str) -> Option<Vec<(&str, Option<Sep>)>> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quote: Option<char> = None;
    let mut chars = cmd.char_indices().peekable();
    let mut prev = ' ';
    let mut comment_at = None;
    while let Some((i, c)) = chars.next() {
        let after_redirect = matches!(prev, '>' | '<');
        // Bash's metacharacters, not Unicode whitespace: a `#` after a CR
        // is a literal.
        let word_start = matches!(
            prev,
            ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>'
        );
        prev = c;
        let sep = match (quote, c) {
            (None, '$') if chars.next_if(|&(_, n)| n == '{').is_some() => {
                loop {
                    match chars.next()?.1 {
                        '}' => break,
                        // A brace, quote or escape inside the expansion may
                        // move its end; the split does not model that.
                        '{' | '\'' | '"' | '\\' | '`' => return None,
                        _ => {}
                    }
                }
                prev = '}';
                continue;
            }
            (None, '<') if cmd[i..].starts_with("<<<") => {
                chars.nth(1);
                continue;
            }
            (None, '<') if cmd[i..].starts_with("<<") => return None,
            (None, '#') if word_start => {
                comment_at = Some(i);
                while chars.next_if(|&(_, n)| n != '\n').is_some() {}
                continue;
            }
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
        segments.push((&cmd[start..comment_at.take().unwrap_or(i)], Some(sep)));
        start = chars.peek().map_or(cmd.len(), |&(next, _)| next);
    }
    segments.push((&cmd[start..comment_at.unwrap_or(cmd.len())], None));
    quote.is_none().then_some(segments)
}

/// Directory a Bash search runs in, as far as a literal `cd <dir>` before
/// the `rg` / `grep` segment decides it: sessions working across repos keep
/// the envelope cwd on another repo, so resolving from it finds no graph.
#[derive(Debug, PartialEq)]
enum CdTarget {
    /// No cd changes the search's shell: none at all, or one in a pipeline,
    /// a subshell or a background job, or one to a missing directory.
    Unchanged,
    Dir(String),
    /// A cd may or may not run (`make || cd x`, `make && cd x; rg`), or its
    /// target cannot be known without expanding it (`cd -`, `$VAR`,
    /// backticks, `~user`, options, a bare `cd`), or a builtin other than cd
    /// moves the shell (`pushd`, `source`, ...), or a quote is left open.
    Unknown,
}

const CWD_BUILTINS: [&str; 8] = [
    "pushd", "popd", "source", ".", "eval", "builtin", "command", "exec",
];

/// Reserved words and `time` that run the command after them in this shell.
const PREFIX_WORDS: [&str; 9] = [
    "!", "time", "if", "then", "elif", "else", "do", "while", "until",
];

fn cd_target(cmd: &str, cwd: &str) -> CdTarget {
    let cdpath = std::env::var_os("CDPATH").is_some_and(|v| !v.is_empty());
    cd_target_with_home(
        cmd,
        cwd,
        std::env::var_os("HOME").as_deref().map(Path::new),
        cdpath,
    )
}

/// `cdpath`: the shell inherits a non-empty `CDPATH`, which may redirect a
/// relative `cd` to another directory.
fn cd_target_with_home(cmd: &str, cwd: &str, home: Option<&Path>, cdpath: bool) -> CdTarget {
    // One substring scan per name keeps a command that moves no directory
    // off the segment parser.
    if !cmd.contains("cd") && !CWD_BUILTINS.iter().any(|b| cmd.contains(b)) {
        return CdTarget::Unchanged;
    }
    let joined = join_continuations(cmd);
    let cmd = joined.as_ref();
    let Some(segments) = shell_segments(cmd) else {
        return CdTarget::Unknown;
    };
    let cdpath = cdpath || cmd.contains("CDPATH");
    let words: Vec<Vec<String>> = segments.iter().map(|&(s, _)| shell_words(s)).collect();
    // `extract_from_shell` takes the first `rg` / `grep` word anywhere
    // (`rtk proxy rg`, `git grep`), so the scan stops at the same word.
    let search = words
        .iter()
        .position(|w| w.iter().any(|w| w == "rg" || w == "grep"));
    // An and-or list ending in `&` runs as a background job: its cd reaches
    // the search only when the search runs in that same job.
    let mut list_end = vec![0; segments.len()];
    let mut end = segments.len();
    for (i, &(_, after)) in segments.iter().enumerate().rev() {
        if matches!(after, None | Some(Sep::Seq | Sep::Background)) {
            end = i;
        }
        list_end[i] = end;
    }
    let search_list = search.map(|k| list_end[k]);
    let mut current: Option<PathBuf> = None;
    let mut before = Some(Sep::Seq);
    let mut subshell = 0usize;
    // After `cd x ||` succeeds, its `||` operand does not run. The operand
    // ends at the next `;`, `&&` or `&` outside any `{ }` / `( )` it opens
    // (`a || b && c` is `(a || b) && c`).
    let mut skipping: Option<usize> = None;
    for (i, (&(segment, after), words)) in segments.iter().zip(&words).enumerate() {
        if Some(i) == search {
            break;
        }
        let ((paren_opens, paren_closes), (brace_opens, brace_closes)) = group_marks(segment);
        if let Some(depth) = skipping.as_mut() {
            *depth =
                (*depth + paren_opens + brace_opens).saturating_sub(paren_closes + brace_closes);
            if *depth == 0 && !matches!(after, Some(Sep::Or | Sep::Pipe)) {
                skipping = None;
            }
            before = after;
            continue;
        }
        let cmd_words: Vec<&str> = words
            .iter()
            .map(String::as_str)
            .skip_while(|w| is_assignment(w))
            .skip_while(|w| *w == "{")
            .collect();
        let starts_subshell = cmd_words.first().is_some_and(|w| w.starts_with('('));
        let in_subshell = subshell > 0 || starts_subshell;
        subshell = (subshell + paren_opens).saturating_sub(paren_closes);
        let other_job =
            segments[list_end[i]].1 == Some(Sep::Background) && Some(list_end[i]) != search_list;
        let own_shell =
            !in_subshell && !other_job && before != Some(Sep::Pipe) && after != Some(Sep::Pipe);
        // `! cd x`, `time . env.sh`, `then cd x`: a prefix the split does
        // not model may still run the command in this shell. A later `.`
        // alone is mostly a path argument (`git add .`).
        let moves = |w: &&str| *w == "cd" || CWD_BUILTINS.contains(w);
        let after_prefix = cmd_words
            .iter()
            .position(|w| !PREFIX_WORDS.contains(w))
            .unwrap_or(cmd_words.len());
        if own_shell
            && (after_prefix > 0 && cmd_words.get(after_prefix).is_some_and(moves)
                || cmd_words.iter().skip(1).any(|w| *w != "." && moves(w)))
        {
            return CdTarget::Unknown;
        }
        match cmd_words.first().copied() {
            Some(builtin) if own_shell && CWD_BUILTINS.contains(&builtin) => {
                return CdTarget::Unknown
            }
            // `set -P` makes a later `cd ..` physical.
            Some("set")
                if own_shell
                    && cmd_words[1..]
                        .iter()
                        .any(|w| w.starts_with('-') && w.contains('P') || *w == "physical") =>
            {
                return CdTarget::Unknown
            }
            Some("cd") if own_shell => {
                let surely_runs = match before {
                    Some(Sep::Seq | Sep::Background) | None => true,
                    Some(Sep::And) => after == Some(Sep::And),
                    _ => false,
                };
                let [_, arg] = cmd_words[..] else {
                    return CdTarget::Unknown;
                };
                if !surely_runs {
                    return CdTarget::Unknown;
                }
                let tilde = unquoted_tilde_arg(segment);
                // Bash searches `CDPATH` for a relative directory whose first
                // component is not `.` or `..` (`.codex` is searched).
                let dot_relative =
                    matches!(arg, "." | "..") || arg.starts_with("./") || arg.starts_with("../");
                if cdpath
                    && !arg.starts_with('/')
                    && !dot_relative
                    && !(tilde && arg.starts_with('~'))
                {
                    return CdTarget::Unknown;
                }
                let base = current.as_deref().unwrap_or(Path::new(cwd));
                match resolve_cd_arg(arg, base, home, tilde) {
                    None => return CdTarget::Unknown,
                    Some(Some(dir)) => {
                        current = Some(dir);
                        if after == Some(Sep::Or) {
                            skipping = Some(0);
                        }
                    }
                    // The cd fails; `cd x || …` then runs its operand,
                    // whose effect the hook does not model.
                    Some(None) if after == Some(Sep::Or) => return CdTarget::Unknown,
                    Some(None) => {}
                }
            }
            _ => {}
        }
        before = after;
    }
    match current {
        None => CdTarget::Unchanged,
        Some(dir) => dir
            .canonicalize()
            .ok()
            .and_then(|dir| dir.into_os_string().into_string().ok())
            .map_or(CdTarget::Unknown, CdTarget::Dir),
    }
}

/// `cmd` with every backslash-newline outside single quotes removed, as
/// bash joins a continued line before it splits words.
fn join_continuations(cmd: &str) -> Cow<'_, str> {
    if !cmd.contains("\\\n") {
        return Cow::Borrowed(cmd);
    }
    let mut out = String::with_capacity(cmd.len());
    let mut chars = cmd.chars().peekable();
    let mut quote = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None | Some('"'), '\\') => {
                if chars.next_if_eq(&'\n').is_some() {
                    continue;
                }
                out.push(c);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
                continue;
            }
            _ => {}
        }
        out.push(c);
    }
    Cow::Owned(out)
}

/// Unquoted `(` / `)` and word `{` / `}` in one segment, as
/// `((paren opens, paren closes), (brace opens, brace closes))`: a quoted
/// `'{'` opens no group.
fn group_marks(segment: &str) -> ((usize, usize), (usize, usize)) {
    let bytes = segment.as_bytes();
    let (mut parens, mut braces) = ((0, 0), (0, 0));
    let mut quote = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match (quote, c) {
            (Some(b'\''), b'\'') | (Some(b'"'), b'"') => quote = None,
            (Some(b'"') | None, b'\\') => i += 1,
            (Some(_), _) => {}
            (None, b'\'' | b'"') => quote = Some(c),
            (None, b'(') => parens.0 += 1,
            (None, b')') => parens.1 += 1,
            (None, b'{' | b'}') => {
                let word = (i == 0 || bytes[i - 1].is_ascii_whitespace())
                    && bytes.get(i + 1).is_none_or(u8::is_ascii_whitespace);
                match (word, c) {
                    (true, b'{') => braces.0 += 1,
                    (true, _) => braces.1 += 1,
                    _ => {}
                }
            }
            _ => {}
        }
        i += 1;
    }
    (parens, braces)
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        let mut chars = name.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Whether the raw `cd` argument starts with an unquoted `~`: `'~/x'` and
/// `"~"` name a directory called `~`.
fn unquoted_tilde_arg(segment: &str) -> bool {
    let mut raw = segment
        .split_whitespace()
        .skip_while(|w| is_assignment(w) || *w == "{");
    raw.next() == Some("cd") && raw.next().is_some_and(|arg| arg.starts_with('~'))
}

/// `None` when the target cannot be known without expanding it, `Some(None)`
/// when it names no directory (the cd fails), `Some(Some(dir))` otherwise.
fn resolve_cd_arg(
    arg: &str,
    base: &Path,
    home: Option<&Path>,
    tilde: bool,
) -> Option<Option<PathBuf>> {
    // A glob expands to a directory the hook does not list.
    if arg.is_empty() || arg.starts_with('-') || arg.contains(['$', '`', '*', '?', '[']) {
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
    // Bash's default `cd -L` drops `..` against the logical path, so
    // `cd /bin; cd ..` lands in `/` even where `/bin` links to `/usr/bin`.
    // Where the logical and the physical walk disagree, bash may fall back
    // from one to the other: unknown.
    let mut logical = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                logical.pop();
            }
            Component::CurDir => {}
            part => logical.push(part),
        }
    }
    match (path.is_dir(), logical.is_dir()) {
        (true, true) => Some(Some(logical)),
        (false, false) => Some(None),
        _ => None,
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
    let joined = join_continuations(cmd);
    let cmd = joined.as_ref();
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
    use super::CdTarget::{Dir, Unchanged, Unknown};
    use super::{
        cd_target_with_home, extract_from_shell, search_terms, shell_segments, CdTarget, Sep,
    };
    use std::path::Path;

    fn canon(p: &Path) -> String {
        p.canonicalize().unwrap().to_str().unwrap().to_string()
    }

    fn target(cmd: &str, cwd: &Path) -> CdTarget {
        cd_target_with_home(cmd, cwd.to_str().unwrap(), None, false)
    }

    fn dir(p: &Path) -> CdTarget {
        Dir(canon(p))
    }

    #[test]
    fn test_cd_target_no_cd_returns_unchanged() {
        let t = tempfile::tempdir().unwrap();
        for cmd in ["grep -rn Foo app", "", "   "] {
            assert_eq!(target(cmd, t.path()), Unchanged, "{cmd:?}");
        }
    }

    #[test]
    fn test_cd_target_quoted_cjk_path_with_spaces_resolves() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("業主 原始碼").join("outsourcing-backend-CN");
        std::fs::create_dir_all(&path).unwrap();
        let d = path.to_str().unwrap();
        let other = tempfile::tempdir().unwrap();
        for cmd in [
            format!("cd \"{d}\" && grep -rn CacheKeyQueue app"),
            format!("cd '{d}' && grep -rn CacheKeyQueue app"),
            format!("cd {} && rg Foo", d.replace(' ', "\\ ")),
        ] {
            assert_eq!(target(&cmd, other.path()), dir(&path), "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_relative_to_envelope_cwd_resolves() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("sub")).unwrap();
        assert_eq!(
            target("cd sub && grep -rn Foo .", t.path()),
            dir(&t.path().join("sub"))
        );
    }

    #[test]
    fn test_cd_target_home_tilde_expands_via_home_only() {
        let h = tempfile::tempdir().unwrap();
        std::fs::create_dir(h.path().join("x")).unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let go =
            |cmd: &str, home| cd_target_with_home(cmd, cwd.path().to_str().unwrap(), home, false);
        assert_eq!(go("cd ~ && grep Foo", Some(h.path())), dir(h.path()));
        assert_eq!(
            go("cd ~/x && grep Foo", Some(h.path())),
            dir(&h.path().join("x"))
        );
        assert_eq!(go("cd ~/x && grep Foo", None), Unknown);
        assert_eq!(go("cd ~bob && grep Foo", Some(h.path())), Unknown);
    }

    #[test]
    fn test_cd_target_unknowable_target_returns_unknown() {
        let t = tempfile::tempdir().unwrap();
        for cmd in [
            "cd - && grep Foo .",
            "cd && grep Foo .",
            "cd $HOME && grep Foo .",
            "cd \"$(pwd)/x\" && grep Foo .",
            "cd `pwd` && grep Foo .",
            "cd -P . && grep Foo .",
            "cd . extra; grep Foo .",
        ] {
            assert_eq!(target(cmd, t.path()), Unknown, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_nonexistent_dir_returns_unchanged() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            target("cd /no/such/dir_zz && grep Foo .", t.path()),
            Unchanged
        );
    }

    #[test]
    fn test_cd_target_last_cd_wins_and_chains_relative() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("a/b")).unwrap();
        std::fs::create_dir(t.path().join("c")).unwrap();
        assert_eq!(
            target("cd a && cd b && grep Foo .", t.path()),
            dir(&t.path().join("a/b"))
        );
        assert_eq!(
            target("cd a && cd ../c && grep Foo .", t.path()),
            dir(&t.path().join("c"))
        );
        // A cd after the search segment does not move the search.
        assert_eq!(
            target("cd a && grep Foo . ; cd ../c", t.path()),
            dir(&t.path().join("a"))
        );
    }

    #[test]
    fn test_cd_target_semicolon_and_newline_separators_resolve() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "cd x; grep Foo .",
            "cd x\ngrep Foo .",
            "make & cd x; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_cd_that_may_not_run_returns_unknown() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        for cmd in [
            "true || cd x; rg Foo",
            "false || cd x && rg Foo",
            "make && cd x; rg Foo",
            "cd x; cd \"$PWD\"; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), Unknown, "{cmd}");
        }
        assert_eq!(
            target("make && cd x && rg Foo", t.path()),
            dir(&t.path().join("x"))
        );
    }

    #[test]
    fn test_cd_target_cd_outside_search_shell_returns_unchanged() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        for cmd in [
            "cd x | rg Foo",
            "cd x & rg Foo",
            "cd x && make & rg Foo",
            "make || cd x & rg Foo",
            "cd x || true & rg Foo",
            "(cd x && make); rg Foo",
            "(make; cd x); rg Foo",
            "echo | pushd x; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), Unchanged, "{cmd}");
        }
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
            assert_eq!(target(cmd, t.path()), Unchanged, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_quoted_tilde_is_a_relative_dir() {
        let h = tempfile::tempdir().unwrap();
        std::fs::create_dir(h.path().join("repo")).unwrap();
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cwd.path().join("~/repo")).unwrap();
        let go = |cmd: &str| {
            cd_target_with_home(cmd, cwd.path().to_str().unwrap(), Some(h.path()), false)
        };
        let literal = dir(&cwd.path().join("~/repo"));
        assert_eq!(go("cd '~/repo'; rg Foo"), literal);
        assert_eq!(go("cd \"~/repo\" && rg Foo"), literal);
        assert_eq!(go("cd ~/repo && rg Foo"), dir(&h.path().join("repo")));
    }

    #[test]
    fn test_cd_target_cwd_builtin_or_open_quote_returns_unknown() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        for cmd in [
            "cd x; cd -; rg Foo",
            "cd x; cd $D; rg Foo",
            "pushd x; rg Foo",
            "cd x; popd; rg Foo",
            "cd x; source env.sh; rg Foo",
            "cd x && grep \"Foo .",
            "cd missing || cd x; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), Unknown, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_or_operand_skipped_after_successful_cd() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("x/y")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "cd x || exit 1; rg Foo",
            "cd x || false && rg Foo",
            "cd x || { echo no; exit 1; }; rg Foo",
            "cd x || { echo failed; cd y; }; rg Foo",
            "cd x || (cd y; exit); rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_quoted_group_mark_in_skipped_operand_opens_no_group() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("x/y")).unwrap();
        let want = dir(&t.path().join("x/y"));
        for cmd in [
            "cd x || echo '{'; cd y; rg Foo",
            "cd x || echo \"(\"; cd y; rg Foo",
            "cd x || echo \\{; cd y; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_comment_text_does_not_hide_a_later_cd() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "echo ready # call foo(\ncd x; rg Foo",
            "echo ready # don't { \ncd x; rg Foo",
            "echo ready #; cd /\ncd x; rg Foo",
            "echo a#b; cd x; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd:?}");
        }
    }

    #[test]
    fn test_cd_target_hash_inside_expansion_or_after_cr_is_literal() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "echo ${v:- #}; cd x; rg Foo",
            "echo ${#PATH}; cd x; rg Foo",
            "echo foo\r#; cd x; rg Foo",
            "cat <<< '#('; cd x; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd:?}");
        }
    }

    #[test]
    fn test_cd_target_heredoc_or_open_expansion_returns_unknown() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        for cmd in [
            "cat <<EOF\n( # )\nEOF\ncd x; rg Foo",
            "echo ${v:-a; cd x; rg Foo",
            "echo ${v:-'}; cd x; #'}; rg Foo",
            "cd x; echo ${v:-{}; cd /; # }\nrg Foo",
            "cd x; ! cd /; rg Foo",
            "cd x; time cd /; rg Foo",
            "cd x; CDPATH=/var; cd tmp; rg Foo",
            "cd x; CDPATH=/var; cd .hidden; rg Foo",
            "cd x; ! . /dev/stdin <<< \"cd /\"; rg Foo",
            "cd x; time . env.sh; rg Foo",
            "cd x; cd /u?r; rg Foo",
            "cd x; set -P; cd ..; rg Foo",
            "cd x; set -o physical; cd ..; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), Unknown, "{cmd:?}");
        }
    }

    #[test]
    fn test_cd_target_parent_of_symlink_is_logical() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("real/inner")).unwrap();
        std::os::unix::fs::symlink(t.path().join("real/inner"), t.path().join("link")).unwrap();
        assert_eq!(target("cd link; cd ..; rg Foo", t.path()), dir(t.path()));
        assert_eq!(target("cd link/..; rg Foo", t.path()), dir(t.path()));
        assert_eq!(target("cd missing/..; rg Foo", t.path()), Unknown);
        assert_eq!(target("cd link/../missing; rg Foo", t.path()), Unchanged);
        // Physical `real/inner/../other` exists, logical `other` does not.
        std::fs::create_dir(t.path().join("real/other")).unwrap();
        assert_eq!(target("cd link/../other; rg Foo", t.path()), Unknown);
    }

    #[test]
    fn test_cd_target_inherited_cdpath_makes_relative_cd_unknown() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        let go = |cmd: &str| cd_target_with_home(cmd, t.path().to_str().unwrap(), None, true);
        assert_eq!(go("cd x; rg Foo"), Unknown);
        assert_eq!(go("cd ./x; rg Foo"), dir(&t.path().join("x")));
    }

    #[test]
    fn test_cd_target_line_continuation_and_prefixed_dot_argument_resolve() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "cd x && \\\nrg Foo",
            "cd \\\nx; rg Foo",
            "cd x && time git diff -- .; rg Foo",
            // Single quotes keep the backslash-newline: the cd fails.
            "cd x; cd 'x\\\ny'; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd:?}");
        }
    }

    #[test]
    fn test_cd_target_cd_in_the_search_background_job_resolves() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "cd x && rg Foo & wait",
            "cd x; rg Foo &",
            "make & cd x && rg Foo &",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_assignments_and_groups_keep_cd() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "X=1; cd x && rg Foo",
            "X=1 cd x && rg Foo",
            "{ cd x; rg Foo; }",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_quoted_tilde_in_search_does_not_block_leading_tilde() {
        let h = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        assert_eq!(
            cd_target_with_home(
                "cd ~ && grep 'cd \"~\"' Foo .",
                cwd.path().to_str().unwrap(),
                Some(h.path()),
                false
            ),
            dir(h.path())
        );
    }

    #[test]
    fn test_cd_target_cd_inside_quotes_is_not_a_segment() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().to_str().unwrap();
        let cmd = format!("echo \"cd {d}\" && grep -rn Foo app");
        assert_eq!(target(&cmd, t.path()), Unchanged);
        let cmd = format!("echo 'a; cd {d}' && grep -rn Foo app");
        assert_eq!(target(&cmd, t.path()), Unchanged);
    }

    #[test]
    fn test_shell_segments_splits_only_unquoted_separators() {
        let segs: Vec<(&str, Option<Sep>)> = shell_segments("cd 'a;b' && grep \"x|y\" . | head")
            .unwrap()
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
            shell_segments("make 2>&1 <&0 &>log >|f; x").unwrap(),
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
