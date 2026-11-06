//! PostToolUse handler: the edit-flow evidence after an edit, a background
//! reindex after a git ref-changing command, and the search delta after a
//! search, merged into one `additionalContext` payload.
//!
//! The search delta answers a `rg` / `grep` / `egrep` / `git grep` Bash call
//! or a Grep tool call with what the graph holds for the pattern's
//! identifiers and the tool output does not show: a definition file, a
//! non-test caller file, or the files of each same-name definition when the
//! output mixes two. It states nothing the output already shows, so a search
//! that found everything gets no payload.
//!
//! Note: orphan-registry sweep used to live here, gated by a `.last-prune`
//! mtime marker on every Bash ToolUse. That introduced two problems:
//! the marker was global state shared across processes (test isolation
//! pain — see hook_post_tool_use_test flake history), and the "1-hour
//! throttle on a side-effect that runs hundreds of times per session"
//! design was over-engineered for a maintenance task that genuinely
//! needs to run once per session, not once per ToolUse. Orphan prune
//! now lives in `session_start::handle` where the trigger frequency
//! matches the work frequency — no marker, no throttle, no global
//! state.

use super::common::{
    ecp_state_dir_ensure, emit_additional_context, lookup_index_dir, strip_shell_quotes, HookInput,
};
use super::pre_tool_use::{bash_command, cd_target, search_call, CdTarget};
use crate::auto_ensure::{ensure_index, EnsureResult};
use crate::background::{spawn_bg, BgJob, BgMarkers};
use crate::commands::graph_csr::iter_incoming_edges_filtered;
use crate::commands::impact::bfs::merged_node_is_test;
use crate::engine::Engine;
use ecp_core::graph::{ArchivedRelType, ArchivedZeroCopyGraph, NodeKind};
use ecp_core::session::view::base_constructors;
use ecp_core::session::MergedGraph;
use ecp_core::EcpError;
use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Upper bound on the search delta block, header included.
const DELTA_MAX_BYTES: usize = 600;
/// The header's share of `DELTA_MAX_BYTES` for the directory it names, so a
/// long path cannot crowd every line out.
const MAX_HEADER_DIR_BYTES: usize = 150;
/// Identifiers of one pattern the delta covers.
const MAX_IDENTIFIERS: usize = 3;
/// Definitions or caller files one identifier lists before `+N more`.
const MAX_LISTED: usize = 5;
const DELTA_HEADER: &str = "ecp graph delta:\n";

/// Git-mutation matcher. Compiled once per process — PostToolUse fires
/// on every Bash tool call so amortising the regex build matters.
///
/// `checkout` / `switch` / `reset` cover branch-switch verbs that change
/// HEAD — without them, the next `ecp` read command would hit
/// `auto_ensure::ensure_fresh` synchronously and pay the cold-rebuild
/// cost (~1-2 s on a mid-size repo) on the user's first query instead of
/// kicking the rebuild to a background spawn here.
fn git_mutation_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"\bgit\s+(commit|merge|rebase|cherry-pick|pull|checkout|switch|reset)(\s|$)",
        )
        .unwrap()
    })
}

pub fn handle(input: &HookInput) -> Result<(), EcpError> {
    // One payload: Claude Code parses one JSON object on stdout, so a second
    // `println!` would be dropped silently.
    let sections: Vec<String> = [
        super::edit_flow::context(input, true),
        maybe_reindex_notice(input),
        search_delta(input),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !sections.is_empty() {
        emit_additional_context("PostToolUse", &sections.join("\n\n"));
    }
    Ok(())
}

fn maybe_reindex_notice(input: &HookInput) -> Option<String> {
    if input.tool_name != "Bash" {
        return None;
    }
    let cmd = input
        .tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !is_git_mutation(cmd) {
        return None;
    }
    let exit = input
        .tool_output
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if exit != 0 {
        return None;
    }

    // No index registered for this worktree → nothing to refresh; the
    // SessionStart hint already nagged the user to run `ecp admin index`.
    let index_dir = lookup_index_dir(&input.cwd)?;
    let repo_root = Path::new(&input.cwd);
    let graph_path = index_dir.join("graph.bin");

    let result = ensure_index(&graph_path, repo_root).unwrap_or(EnsureResult::Missing);
    let age = match result {
        EnsureResult::Stale { age_seconds, .. } => age_seconds,
        _ => return None,
    };

    // Marker/log/lock live in the hook-local state dir so they're
    // scoped to the worktree, not shared across all worktrees that
    // happen to point at the same `~/.ecp/<repo>__<hash>/commits/<sha>/`.
    let state_dir = ecp_state_dir_ensure(&input.cwd)?;
    if !spawn_background_reindex(repo_root, &state_dir) {
        return None;
    }
    Some(format!(
        "ecp reindex started in background (index stale ~{age}s). Subsequent ecp tools may use stale data until completion (~30-120s). If it appears stuck, run `ecp admin index` manually."
    ))
}

fn is_git_mutation(cmd: &str) -> bool {
    git_mutation_re().is_match(&strip_shell_quotes(cmd))
}

/// Detached background `ecp admin index --repo <cwd>` under flock at
/// `<state_dir>/.analyze.lock`. Writes `.rebuild-complete` on success
/// or `.rebuild-failed` after MAX=3 attempts. Returns true iff the
/// launcher subprocess was spawned (the analyze outcome surfaces
/// asynchronously via marker files consumed by UserPromptSubmit).
fn spawn_background_reindex(repo_root: &Path, state_dir: &Path) -> bool {
    let repo_str = repo_root.to_string_lossy();
    let lock = state_dir.join(".analyze.lock");
    let complete = state_dir.join(".rebuild-complete");
    let failed = state_dir.join(".rebuild-failed");
    let log = state_dir.join("last-rebuild.log");

    let args: Vec<&str> = vec!["admin", "index", "--repo", repo_str.as_ref()];

    spawn_bg(BgJob {
        args: &args,
        lock: &lock,
        cwd: repo_root,
        retry: (3, 2),
        markers: Some(BgMarkers {
            log: &log,
            complete: &complete,
            failed: &failed,
        }),
        then_args: None,
        env: &[],
    })
}

/// The search a tool call ran, as far as the delta needs it.
struct Search {
    /// The pattern's identifier-shaped words ([`identifiers`]).
    words: Vec<String>,
    /// The directory the search ran in: relative output paths start here.
    dir: PathBuf,
    /// The directory whose graph answers: the one directory operand, else
    /// `dir`.
    graph_dir: PathBuf,
}

/// The delta block for a search call, `None` when it adds nothing. Returns
/// before any graph load for a call that is no search, has no identifier,
/// reads only regular files or stdin, or whose output is absent.
fn search_delta(input: &HookInput) -> Option<String> {
    let search = search_of(input)?;
    let output = search_output(&input.tool_output)?;
    let index_dir = lookup_index_dir(search.graph_dir.to_str()?)?;
    let engine = Engine::load(index_dir.join("graph.bin")).ok()?;
    let graph = engine.graph().ok()?;
    let top = crate::read_context::worktree_top(&search.graph_dir);
    let top = dunce::canonicalize(&top).unwrap_or(top);
    // The delta reads base-graph edges, and the graph may be a sibling
    // worktree's. A fact stands only while its file still holds the indexed
    // text, so a rename, a delete or a file this tree lacks drops it. The
    // gate runs before the identifier cap: a stale name must not take a slot.
    let mut recorded: FxHashMap<String, bool> = FxHashMap::default();
    let mut holds = |file: &str| -> bool {
        match recorded.get(file) {
            Some(&known) => known,
            None => {
                let known = engine.file_text_is_base_recorded(&top, file);
                recorded.insert(file.to_owned(), known);
                known
            }
        }
    };
    let found: Vec<(String, Vec<Definition<'_>>)> = search
        .words
        .into_iter()
        .filter_map(|word| {
            let defs: Vec<Definition<'_>> = definitions(graph, &word)
                .into_iter()
                .filter_map(|mut d| {
                    holds(d.file).then(|| {
                        d.callers.retain(|file| holds(file));
                        d
                    })
                })
                .collect();
            (!defs.is_empty()).then_some((word, defs))
        })
        .take(MAX_IDENTIFIERS)
        .collect();
    if found.is_empty() {
        return None;
    }
    let prefix = dunce::canonicalize(&search.dir)
        .ok()
        .and_then(|dir| {
            dir.strip_prefix(&top).ok().map(|rel| {
                rel.components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/")
            })
        })
        .unwrap_or_default();
    let relevant: FxHashSet<&str> = found
        .iter()
        .flat_map(|(_, defs)| defs)
        .flat_map(|d| std::iter::once(d.file).chain(d.callers.iter().copied()))
        .collect();
    let mentioned = mentioned_files(&output.text, &output.files, &prefix, &relevant);
    let lines: Vec<DeltaLine> = found
        .iter()
        .flat_map(|(name, defs)| delta_lines(name, defs, &mentioned))
        .collect();
    let header = match dunce::canonicalize(&input.cwd) {
        Ok(cwd) if cwd == top => Cow::Borrowed(DELTA_HEADER),
        _ => Cow::Owned(format!(
            "ecp graph delta in {}:\n",
            tail_within(&top.to_string_lossy(), MAX_HEADER_DIR_BYTES)
        )),
    };
    render_delta(&header, &lines)
}

/// The search of a Bash command or a Grep tool call. `None` for any other
/// call, a pattern with no identifier, a `cd` the hook cannot place, a search
/// of stdin, and a search whose every path operand is one regular file.
fn search_of(input: &HookInput) -> Option<Search> {
    let (pattern, operands, cmd) = match input.tool_name.as_str() {
        "Grep" => {
            let pattern = input.tool_input.get("pattern")?.as_str()?.to_owned();
            let path = input
                .tool_input
                .get("path")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty());
            (pattern, path.map(str::to_owned).into_iter().collect(), None)
        }
        "Bash" => {
            let cmd = bash_command(&input.tool_input)?;
            let call = search_call(cmd).filter(|call| !call.reads_stdin)?;
            (call.pattern, call.operands, Some(cmd))
        }
        _ => return None,
    };
    let words = identifiers(&pattern);
    if words.is_empty() {
        return None;
    }
    let dir = match cmd.map(|cmd| cd_target(cmd, &input.cwd)) {
        None | Some(CdTarget::Unchanged) => PathBuf::from(&input.cwd),
        Some(CdTarget::Dir(dir)) => PathBuf::from(dir),
        // A delta from a repo the search may not run in would mislead.
        Some(CdTarget::Unknown) => return None,
    };
    let home = std::env::var_os("HOME");
    let searched: Vec<PathBuf> = operands
        .iter()
        .map(|op| operand_path(&dir, op, home.as_deref().map(Path::new)))
        .collect();
    if !searched.is_empty() && searched.iter().all(|path| path.is_file()) {
        return None;
    }
    let graph_dir = match &searched[..] {
        [only] if only.is_dir() => only.clone(),
        _ => dir.clone(),
    };
    Some(Search {
        words,
        dir,
        graph_dir,
    })
}

/// The path an operand names: quotes stripped, a leading `~/` expanded with
/// `home` (the hook sees the command before the shell expands it), else
/// `dir` joined with it.
fn operand_path(dir: &Path, operand: &str, home: Option<&Path>) -> PathBuf {
    let operand = operand.trim_matches(['"', '\'']);
    match (operand.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => dir.join(operand),
    }
}

/// The identifier-shaped words of a grep / rg pattern, in pattern order and
/// each once. Regex syntax carries no symbol name: `\bfetch\s*\(` must yield
/// `fetch`, not `bfetch`. Escapes (`\X`) are dropped whole, every other
/// non-word character splits, and a word survives only with three or more
/// characters, a letter among them, and no leading digit.
fn identifiers(pattern: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut flush = |current: &mut String| {
        let word = std::mem::take(current);
        if word.chars().count() >= 3
            && word.chars().any(char::is_alphabetic)
            && !word.starts_with(|c: char| c.is_numeric())
            && !words.contains(&word)
        {
            words.push(word);
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
    words
}

/// What a search printed: its text, and the file names a Grep tool response
/// lists as a structured field (a name may hold spaces, which no text split
/// can recover).
struct SearchOutput<'a> {
    text: Cow<'a, str>,
    files: Vec<&'a str>,
}

/// The output of a search: a string `tool_output`, a Bash `stdout`, or a Grep
/// tool's `filenames` and `content`. `None` when the output is absent or has
/// none of these shapes: the delta then has no evidence.
fn search_output(output: &Value) -> Option<SearchOutput<'_>> {
    let (text, files) = match output {
        Value::String(text) => (text.as_str(), Vec::new()),
        Value::Object(map) => {
            if let Some(stdout) = map.get("stdout").and_then(Value::as_str) {
                (stdout, Vec::new())
            } else {
                let filenames = map.get("filenames").and_then(Value::as_array);
                let content = map.get("content").and_then(Value::as_str);
                if filenames.is_none() && content.is_none() {
                    return None;
                }
                let files = filenames
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                (content.unwrap_or_default(), files)
            }
        }
        _ => return None,
    };
    Some(SearchOutput {
        text: Cow::Borrowed(text),
        files,
    })
}

/// The path before the first `:<digits>` that ends the line or precedes
/// another `:` (`path:12:text`, `path:12:5:text`, `grep -c`'s `path:3`): a
/// path may hold spaces, so no whitespace split finds it.
fn line_path(line: &str) -> Option<&str> {
    line.match_indices(':').find_map(|(colon, _)| {
        let rest = &line[colon + 1..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        (digits > 0 && matches!(rest.as_bytes().get(digits), None | Some(b':')))
            .then_some(&line[..colon])
    })
}

/// The paths of `relevant` that the output mentions: each name in `files`, and
/// per line of `text` the `path:line:` prefix when there is one, else the
/// whole line and each token split at whitespace and `:`. A path is mentioned
/// when it equals the path or ends with `/` and the path (an absolute path),
/// or when `prefix/path` equals it: a search in a subdirectory prints paths
/// relative to that subdirectory. `\` reads as `/`.
fn mentioned_files<'g>(
    text: &str,
    files: &[&str],
    prefix: &str,
    relevant: &FxHashSet<&'g str>,
) -> FxHashSet<&'g str> {
    let mut seen = FxHashSet::default();
    let mut joined = String::new();
    let mut probe_path = |path: &str| {
        let path = path.trim();
        let path = if path.contains('\\') {
            Cow::Owned(path.replace('\\', "/"))
        } else {
            Cow::Borrowed(path)
        };
        let path = path.trim_start_matches("./");
        if path.is_empty() {
            return;
        }
        let mut probe = |candidate: &str| {
            if let Some(&file) = relevant.get(candidate) {
                seen.insert(file);
            }
        };
        probe(path);
        for (slash, _) in path.match_indices('/') {
            probe(&path[slash + 1..]);
        }
        if !prefix.is_empty() && !path.starts_with('/') {
            joined.clear();
            joined.push_str(prefix);
            joined.push('/');
            joined.push_str(path);
            probe(&joined);
        }
    };
    for file in files {
        probe_path(file);
    }
    for line in text.split(['\n', '\0']) {
        if let Some(path) = line_path(line) {
            probe_path(path);
            continue;
        }
        probe_path(line);
        for token in line.split(|c: char| c.is_whitespace() || c == ':') {
            probe_path(token);
        }
    }
    seen
}

/// One definition of a searched name, with the files of its non-test direct
/// callers.
struct Definition<'g> {
    file: &'g str,
    line: u32,
    kind: &'static str,
    callers: BTreeSet<&'g str>,
}

/// Every definition named exactly `name`, ordered by file and line. A caller
/// is the source of a `Calls` edge into the definition or its constructors,
/// the edges `find`'s private `call_sources` follows, kept as a file once
/// however many sources it holds, under the test rule `impact` walks by.
fn definitions<'g>(graph: &'g ArchivedZeroCopyGraph, name: &str) -> Vec<Definition<'g>> {
    let merged = MergedGraph::new(graph, None);
    let file_of = move |idx: u32| -> Option<&'g str> {
        let node = &graph.nodes[idx as usize];
        node.has_owning_file().then(|| {
            graph.files[node.file_idx.to_native() as usize]
                .path
                .resolve(&graph.string_pool)
        })
    };
    let mut defs: Vec<Definition<'g>> = graph
        .nodes_by_name(name)
        .filter_map(|idx| {
            let node = &graph.nodes[idx as usize];
            let kind = NodeKind::from(&node.kind);
            if !names_a_symbol(kind) {
                return None;
            }
            let file = file_of(idx)?;
            let callers = std::iter::once(idx)
                .chain(base_constructors(graph, idx))
                .flat_map(|target| {
                    iter_incoming_edges_filtered(graph, target, |rel| {
                        matches!(rel, ArchivedRelType::Calls)
                    })
                })
                .filter(|&(source, _)| !merged_node_is_test(&merged, source))
                .filter_map(|(source, _)| file_of(source))
                .collect();
            Some(Definition {
                file,
                line: node.start_line(),
                kind: kind.as_str(),
                callers,
            })
        })
        .collect();
    defs.sort_by(|a, b| (a.file, a.line).cmp(&(b.file, b.line)));
    defs
}

/// Whether a node of `kind` declares a symbol a search can name: not a file,
/// an import, a route, a document section or a path literal.
fn names_a_symbol(kind: NodeKind) -> bool {
    !matches!(
        kind,
        NodeKind::File
            | NodeKind::Import
            | NodeKind::Route
            | NodeKind::Process
            | NodeKind::Document
            | NodeKind::Section
            | NodeKind::EntryPoint
            | NodeKind::PathLiteral
    )
}

/// One line of the delta block. `callers_of` names the identifier of a line
/// that lists caller files, for the `ecp impact` command.
struct DeltaLine {
    text: String,
    callers_of: Option<String>,
}

impl DeltaLine {
    fn plain(text: String) -> Self {
        Self {
            text,
            callers_of: None,
        }
    }
}

/// What `defs` of `name` hold that `mentioned` lacks: each definition whose
/// file is not mentioned, the non-test caller files not mentioned, and, when
/// the output mentions the files of two or more definitions, which of the
/// mentioned files belong to which definition.
fn delta_lines(name: &str, defs: &[Definition<'_>], mentioned: &FxHashSet<&str>) -> Vec<DeltaLine> {
    let mut lines = Vec::new();
    let (shown, unseen): (Vec<&Definition<'_>>, Vec<&Definition<'_>>) =
        defs.iter().partition(|d| mentioned.contains(d.file));
    for def in unseen.iter().take(MAX_LISTED) {
        lines.push(DeltaLine::plain(format!(
            "{name} → {}:{} [{}]",
            def.file, def.line, def.kind
        )));
    }
    if unseen.len() > MAX_LISTED {
        lines.push(DeltaLine::plain(format!(
            "{name}: +{} more definitions",
            unseen.len() - MAX_LISTED
        )));
    }
    let missing: BTreeSet<&str> = defs
        .iter()
        .flat_map(|d| d.callers.iter().copied())
        .filter(|file| !mentioned.contains(file))
        .collect();
    if !missing.is_empty() {
        let listed: Vec<&str> = missing.iter().take(MAX_LISTED).copied().collect();
        let more = match missing.len() - listed.len() {
            0 => String::new(),
            n => format!(", +{n} more"),
        };
        lines.push(DeltaLine {
            text: format!(
                "{name} callers outside the output: {}{more}",
                listed.join(", ")
            ),
            callers_of: Some(name.to_owned()),
        });
    }
    if shown.len() >= 2 {
        let map: Vec<String> = shown
            .iter()
            .map(|def| {
                let files: Vec<&str> = std::iter::once(def.file)
                    .chain(
                        def.callers
                            .iter()
                            .copied()
                            .filter(|f| *f != def.file && mentioned.contains(f)),
                    )
                    .collect();
                format!(
                    "{}:{} [{}] ← {}",
                    def.file,
                    def.line,
                    def.kind,
                    files.join(", ")
                )
            })
            .collect();
        lines.push(DeltaLine::plain(format!(
            "{name} has {} definitions in the output: {}",
            shown.len(),
            map.join("; ")
        )));
    }
    lines
}

/// `header`, then each line that fits, within `DELTA_MAX_BYTES`. The cut
/// falls on a line boundary and keeps the order. The `ecp impact` command of
/// the first kept caller line follows the lines, once; its bytes are kept
/// free from that line on. `None` when no line fits.
fn render_delta(header: &str, lines: &[DeltaLine]) -> Option<String> {
    let mut out = String::from(header);
    let mut hint: Option<String> = None;
    for line in lines {
        let new_hint = match (&hint, &line.callers_of) {
            (None, Some(name)) => Some(format!(
                "  ecp impact --target {name} --direction upstream\n"
            )),
            _ => None,
        };
        let reserved = hint.as_ref().or(new_hint.as_ref()).map_or(0, String::len);
        if out.len() + line.text.len() + 3 + reserved > DELTA_MAX_BYTES {
            break;
        }
        out.push_str("  ");
        out.push_str(&line.text);
        out.push('\n');
        hint = hint.or(new_hint);
    }
    if out.len() == header.len() {
        return None;
    }
    out.push_str(hint.as_deref().unwrap_or_default());
    Some(out)
}

/// `text` when it fits in `max_bytes`, else `…` and the longest tail that
/// fits, cut on a char boundary: a path's tail names the directory.
fn tail_within(text: &str, max_bytes: usize) -> Cow<'_, str> {
    const ELLIPSIS: &str = "…";
    if text.len() <= max_bytes {
        return Cow::Borrowed(text);
    }
    let mut start = text.len() - max_bytes.saturating_sub(ELLIPSIS.len()).min(text.len());
    while !text.is_char_boundary(start) {
        start += 1;
    }
    Cow::Owned(format!("{ELLIPSIS}{}", &text[start..]))
}

#[cfg(test)]
mod tests {
    use super::{
        delta_lines, identifiers, is_git_mutation, line_path, mentioned_files, operand_path,
        render_delta, search_output, tail_within, Definition, DeltaLine, DELTA_HEADER,
        DELTA_MAX_BYTES,
    };
    use rustc_hash::FxHashSet;
    use serde_json::json;
    use std::collections::BTreeSet;

    fn def<'a>(file: &'a str, line: u32, callers: &[&'a str]) -> Definition<'a> {
        Definition {
            file,
            line,
            kind: "Function",
            callers: callers.iter().copied().collect::<BTreeSet<_>>(),
        }
    }

    fn set<'a>(files: &[&'a str]) -> FxHashSet<&'a str> {
        files.iter().copied().collect()
    }

    fn texts(lines: &[DeltaLine]) -> Vec<&str> {
        lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn test_identifiers_regex_alternation_splits_into_words() {
        assert_eq!(
            identifiers("(compute_single|score|bm25)"),
            ["compute_single", "score", "bm25"]
        );
        assert_eq!(identifiers("HookInput"), ["HookInput"]);
        assert_eq!(
            identifiers("pub struct HookInput"),
            ["pub", "struct", "HookInput"]
        );
    }

    #[test]
    fn test_identifiers_escapes_short_words_and_leading_digits_are_dropped() {
        assert_eq!(identifiers(r"\bfetch\s*\("), ["fetch"]);
        assert_eq!(identifiers("fn .*uid"), ["uid"]);
        for pattern in [r"^\[t\]", "12345", "ab", "9abc", r"\d+", ""] {
            assert!(identifiers(pattern).is_empty(), "{pattern:?}");
        }
    }

    #[test]
    fn test_identifiers_repeated_word_is_kept_once() {
        assert_eq!(
            identifiers("load_user|load_user\\(|_private"),
            ["load_user", "_private"]
        );
    }

    fn text_of(output: &serde_json::Value) -> Option<(String, Vec<String>)> {
        search_output(output).map(|o| {
            (
                o.text.into_owned(),
                o.files.into_iter().map(str::to_owned).collect(),
            )
        })
    }

    #[test]
    fn test_search_output_each_shape_returns_its_text_and_files_and_absent_returns_none() {
        assert_eq!(
            text_of(&json!("a.py:1:x")),
            Some(("a.py:1:x".into(), vec![]))
        );
        assert_eq!(
            text_of(&json!({"stdout": "a.py:1:x", "stderr": "b.py"})),
            Some(("a.py:1:x".into(), vec![]))
        );
        assert_eq!(
            text_of(&json!({"filenames": ["/r/my a.py"], "content": "/r/b.py:2:x"})),
            Some(("/r/b.py:2:x".into(), vec!["/r/my a.py".into()]))
        );
        assert_eq!(
            text_of(&json!({"filenames": []})),
            Some((String::new(), vec![]))
        );
        assert_eq!(text_of(&json!("")), Some((String::new(), vec![])));
        for absent in [json!(null), json!({"exit_code": 0}), json!(3)] {
            assert_eq!(text_of(&absent), None, "{absent}");
        }
    }

    #[test]
    fn test_line_path_takes_the_prefix_before_the_line_number() {
        assert_eq!(line_path("src/my module.py:3:x"), Some("src/my module.py"));
        assert_eq!(line_path("a.py:12:5:x"), Some("a.py"));
        assert_eq!(line_path("a b.py:3"), Some("a b.py"));
        assert_eq!(line_path(r"C:\r\a.py:7:x"), Some(r"C:\r\a.py"));
        for none in ["", "src/a.py", "a.py:3x:y", "a.py-3-x"] {
            assert_eq!(line_path(none), None, "{none:?}");
        }
    }

    #[test]
    fn test_mentioned_files_relative_absolute_prefixed_and_dot_slash_tokens_match() {
        let relevant = set(&["models.py", "src/api.py", "pkg/sub/c.ts", "deep/d.php"]);
        let output = "./models.py:1:x\n/home/u/repo/src/api.py:3:y\nc.ts:4:z\nnot/deep/d.phpx\n";
        assert_eq!(
            mentioned_files(output, &[], "pkg/sub", &relevant),
            set(&["models.py", "src/api.py", "pkg/sub/c.ts"])
        );
        assert!(mentioned_files("", &[], "", &relevant).is_empty());
    }

    #[test]
    fn test_mentioned_files_backslash_separators_match_as_slashes() {
        let relevant = set(&["src/api.py", "pkg/c.ts"]);
        let output = "C:\\repo\\src\\api.py:3:y\nc.ts:4:z\n";
        assert_eq!(
            mentioned_files(output, &[], "pkg", &relevant),
            set(&["src/api.py", "pkg/c.ts"])
        );
        assert_eq!(
            mentioned_files(".\\src\\api.py\n", &[], "", &relevant),
            set(&["src/api.py"])
        );
    }

    #[test]
    fn test_mentioned_files_path_with_spaces_matches_in_text_and_filenames() {
        let relevant = set(&["src/my module.py", "module.py", "other dir/b.py"]);
        assert_eq!(
            mentioned_files("src/my module.py:3:def f\n", &[], "", &relevant),
            set(&["src/my module.py"]),
            "the path prefix is one token; `module.py` is not mentioned"
        );
        assert_eq!(
            mentioned_files("other dir/b.py\n", &[], "", &relevant),
            set(&["other dir/b.py"]),
            "a files-only line is one path"
        );
        assert_eq!(
            mentioned_files("", &["/r/src/my module.py"], "", &relevant),
            set(&["src/my module.py"])
        );
    }

    #[test]
    fn test_operand_path_expands_home_and_strips_quotes() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let dir = tmp.path().join("work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(home.join("a.py"), "").unwrap();
        std::fs::write(dir.join("b.py"), "").unwrap();
        assert!(operand_path(&dir, "~/a.py", Some(&home)).is_file());
        assert!(operand_path(&dir, "\"b.py\"", Some(&home)).is_file());
        assert!(operand_path(&dir, "'b.py'", None).is_file());
        assert!(!operand_path(&dir, "~/a.py", None).is_file(), "no home");
    }

    #[test]
    fn test_delta_lines_every_file_mentioned_returns_nothing() {
        let defs = [def("a.py", 1, &["b.py", "a.py"])];
        assert!(delta_lines("f", &defs, &set(&["a.py", "b.py"])).is_empty());
    }

    #[test]
    fn test_delta_lines_unmentioned_files_are_named_and_capped() {
        let callers = [
            "c1.py", "c2.py", "c3.py", "c4.py", "c5.py", "c6.py", "c7.py",
        ];
        let defs = [def("a.py", 3, &callers)];
        let lines = delta_lines("f", &defs, &set(&["c1.py"]));
        assert_eq!(
            texts(&lines),
            [
                "f → a.py:3 [Function]",
                "f callers outside the output: c2.py, c3.py, c4.py, c5.py, c6.py, +1 more",
            ]
        );
        assert_eq!(lines[0].callers_of, None);
        assert_eq!(lines[1].callers_of.as_deref(), Some("f"));
    }

    #[test]
    fn test_delta_lines_two_shown_definitions_map_their_files() {
        let defs = [def("a.py", 1, &["x.py", "y.py"]), def("b.ts", 9, &[])];
        let lines = delta_lines("f", &defs, &set(&["a.py", "b.ts", "x.py", "y.py"]));
        assert_eq!(
            texts(&lines),
            ["f has 2 definitions in the output: a.py:1 [Function] ← a.py, x.py, y.py; b.ts:9 [Function] ← b.ts"]
        );
    }

    fn line(text: &str, callers_of: Option<&str>) -> DeltaLine {
        DeltaLine {
            text: text.to_owned(),
            callers_of: callers_of.map(str::to_owned),
        }
    }

    #[test]
    fn test_render_delta_over_cap_cuts_at_line_boundary_and_keeps_hint() {
        let filler = "x".repeat(100);
        let mut lines: Vec<DeltaLine> = (0..3)
            .map(|i| line(&format!("{i}{filler}"), None))
            .collect();
        lines.push(line(
            &format!("f callers outside the output: {filler}"),
            Some("f"),
        ));
        lines.extend((3..9).map(|i| line(&format!("{i}{filler}"), None)));
        let out = render_delta(DELTA_HEADER, &lines).unwrap();
        assert!(out.len() <= DELTA_MAX_BYTES, "{} bytes", out.len());
        let body: Vec<&str> = out.lines().collect();
        assert_eq!(body[0], DELTA_HEADER.trim_end());
        assert_eq!(
            body.last().copied(),
            Some("  ecp impact --target f --direction upstream")
        );
        assert_eq!(out.matches("ecp impact").count(), 1);
        for (kept, want) in body[1..body.len() - 1].iter().zip(&lines) {
            assert_eq!(*kept, format!("  {}", want.text));
        }
        assert!(body.len() - 2 < lines.len(), "nothing was cut: {out}");
    }

    #[test]
    fn test_render_delta_no_line_returns_none() {
        assert_eq!(render_delta(DELTA_HEADER, &[]), None);
        let long = line(&"y".repeat(DELTA_MAX_BYTES), None);
        assert_eq!(render_delta(DELTA_HEADER, &[long]), None);
    }

    #[test]
    fn test_tail_within_long_non_ascii_path_keeps_a_char_boundary_tail() {
        let path = format!("/{}/倉庫", "目錄".repeat(40));
        let tail = tail_within(&path, 20);
        assert!(tail.len() <= 20, "{tail}");
        assert!(tail.starts_with('…') && tail.ends_with("倉庫"), "{tail}");
        assert_eq!(tail_within("/short", 20), "/short");
    }

    #[test]
    fn commit_verbs_match() {
        for cmd in [
            "git commit -m foo",
            "git merge feature",
            "git rebase main",
            "git cherry-pick abc123",
            "git pull",
        ] {
            assert!(is_git_mutation(cmd), "expected match: {cmd}");
        }
    }

    #[test]
    fn branch_switch_verbs_match() {
        for cmd in [
            "git checkout main",
            "git checkout -b feat/x",
            "git switch develop",
            "git switch -c new-branch",
            "git reset --hard HEAD~1",
        ] {
            assert!(is_git_mutation(cmd), "expected match: {cmd}");
        }
    }

    #[test]
    fn non_mutating_verbs_dont_match() {
        for cmd in [
            "git status",
            "git log --oneline",
            "git diff HEAD",
            "git fetch origin",
            "git branch -a",
            "ls -la",
            "echo \"git commit -m foo\"",
        ] {
            assert!(!is_git_mutation(cmd), "expected no match: {cmd}");
        }
    }
}
