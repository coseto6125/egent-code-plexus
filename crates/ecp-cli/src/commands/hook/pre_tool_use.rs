//! PreToolUse handler: extract a search pattern from Grep / Glob / Bash
//! invocations, run an in-process `ecp find --mode bm25`, and inject
//! the top-K hits into the conversation as `additionalContext`. Capped
//! at 5 hits or ~2 KB serialized to keep the token cost bounded.
//!
//! A hit names a file:line only when the working-tree file holds the text
//! the line came from (the overlay fragment's, else the indexed graph's).
//! The hits of any other file are dropped, an `omitted:` line names the
//! file, and one background `ecp find` refreshes the session overlay.

use super::common::{emit_additional_context, lookup_index_dir, HookInput};
use crate::background::{spawn_bg, BgJob, BgMarkers};
use crate::commands::find::{compute_hits_limited, FindArgs, FindMode, Hit};
use crate::engine::Engine;
use ecp_core::session::SessionState;
use ecp_core::EcpError;
use std::borrow::Cow;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

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
    let top = crate::read_context::worktree_top(Path::new(search_dir));
    let sid = crate::session::resolver::hook_session_id(&input.session_id);
    let engine = with_dirty_overlay(
        Engine::load(&graph_path).ok()?,
        &index_dir,
        &top,
        sid.as_deref(),
    );
    let args = FindArgs {
        pattern: Some(pattern.clone()),
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
    let hits = compute_hits_limited(args, &engine, MAX_HITS).ok()?;
    if hits.is_empty() {
        return None;
    }
    let (hits, dropped) = keep_recorded_hits(hits, |file| engine.file_text_is_recorded(&top, file));
    let omitted = omitted_line(&dropped.files, dropped.hits);
    let mut lines = if hits.is_empty() {
        String::new()
    } else {
        render_hits(&hits, MAX_BYTES.saturating_sub(omitted.len()))
    };
    if lines.is_empty() && dropped.hits > 0 {
        lines.push_str(HITS_HEADER);
    }
    if lines.is_empty() {
        return None;
    }
    lines.push_str(&omitted);
    if dropped.hits > 0 {
        if let (Some(sid), Some(repo_root)) = (&sid, index_dir.parent().and_then(Path::parent)) {
            maybe_spawn_overlay_refresh(repo_root, &top, sid, &pattern, &dropped.files);
        }
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

/// The hits of the files the working tree still holds as ecp read them, and
/// the rest. A hit with no file names no file:line and stays. The proof runs
/// once per distinct file; a dropped hit would name a line of text that is no
/// longer on disk.
fn keep_recorded_hits(
    hits: Vec<Hit>,
    mut is_recorded: impl FnMut(&str) -> bool,
) -> (Vec<Hit>, Dropped) {
    let mut verdicts: Vec<(String, bool)> = Vec::new();
    let mut dropped = Dropped::default();
    let kept = hits
        .into_iter()
        .filter(|hit| {
            if hit.file.is_empty() {
                return true;
            }
            let recorded = match verdicts.iter().find(|(file, _)| *file == hit.file) {
                Some(&(_, recorded)) => recorded,
                None => {
                    let recorded = is_recorded(&hit.file);
                    verdicts.push((hit.file.clone(), recorded));
                    if !recorded {
                        dropped.files.push(hit.file.clone());
                    }
                    recorded
                }
            };
            if !recorded {
                dropped.hits += 1;
            }
            recorded
        })
        .collect();
    (kept, dropped)
}

/// What `keep_recorded_hits` removed: how many hits, in which files (each
/// once, in hit order).
#[derive(Default)]
struct Dropped {
    hits: usize,
    files: Vec<String>,
}

/// The line that tells the agent a symbol may exist although no line is
/// offered for it. Empty when nothing dropped. Names two files, then counts
/// the rest.
fn omitted_line(files: &[String], hits: usize) -> String {
    if hits == 0 {
        return String::new();
    }
    let mut names = files.iter().take(2).cloned().collect::<Vec<_>>().join(", ");
    if files.len() > 2 {
        let _ = write!(names, ", +{} more", files.len() - 2);
    }
    format!("  omitted: {hits} hit(s) in {names} (file changed since ecp last read it)\n")
}

/// Starts one background `ecp find` that rewrites this session's overlay
/// from the changed files, so the next hook call proves their hits. Never
/// without a session the hook was told of: the per-process fallback id names
/// a dir nobody reads.
fn maybe_spawn_overlay_refresh(
    repo_root: &Path,
    top: &Path,
    sid: &str,
    pattern: &str,
    dropped_files: &[String],
) {
    if crate::auto_ensure::background_rebuild_skipped()
        || ecp_core::registry::validate_cache_component(sid).is_err()
    {
        return;
    }
    // Per worktree: a sibling's refresh marker must not suppress this one's.
    let markers = RefreshMarkers::under(
        repo_root,
        &crate::session::resolver::session_dir_name(sid, top),
    );
    let newest_change = dropped_files
        .iter()
        .filter_map(|file| fs::metadata(top.join(file)).and_then(|m| m.modified()).ok())
        .max();
    spawn_overlay_refresh_with(&markers, top, sid, pattern, newest_change, spawn_bg);
}

/// The refresh's lock and outcome files. They sit beside `sessions/`, never
/// in it, so no session enumerator lists them.
struct RefreshMarkers {
    dir: PathBuf,
    lock: PathBuf,
    log: PathBuf,
    complete: PathBuf,
    failed: PathBuf,
}

impl RefreshMarkers {
    fn under(repo_root: &Path, sid: &str) -> Self {
        let dir = repo_root.join(".hook-refresh");
        Self {
            lock: dir.join(format!("{sid}.lock")),
            log: dir.join(format!("{sid}.log")),
            complete: dir.join(format!("{sid}.complete")),
            failed: dir.join(format!("{sid}.failed")),
            dir,
        }
    }

    /// Whether a refresh already ran after `newest_change`. A file that no
    /// refresh can make provable (an LFS file, whose `git archive` bytes skip
    /// the filter driver) then does not respawn the job on every call.
    fn ran_since(&self, newest_change: Option<SystemTime>) -> bool {
        let finished = [&self.complete, &self.failed]
            .into_iter()
            .filter_map(|marker| fs::metadata(marker).and_then(|m| m.modified()).ok())
            .max();
        match (finished, newest_change) {
            (Some(done), Some(changed)) => done >= changed,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }
}

fn spawn_overlay_refresh_with(
    markers: &RefreshMarkers,
    top: &Path,
    sid: &str,
    pattern: &str,
    newest_change: Option<SystemTime>,
    spawn: impl FnOnce(BgJob<'_>) -> bool,
) -> bool {
    if markers.ran_since(newest_change) || fs::create_dir_all(&markers.dir).is_err() {
        return false;
    }
    let top_str = top.to_string_lossy();
    let args = [
        "find",
        "--repo",
        top_str.as_ref(),
        "--format",
        "json",
        "--",
        pattern,
    ];
    spawn(BgJob {
        args: &args,
        lock: &markers.lock,
        cwd: top,
        retry: (1, 0),
        markers: Some(BgMarkers {
            log: &markers.log,
            complete: &markers.complete,
            failed: &markers.failed,
        }),
        then_args: None,
        env: &[("ECP_SESSION_ID", sid)],
    })
}

/// Attaches this session's overlay, so a hit from a file edited since the
/// build shows the line the working tree has, a deleted symbol drops out and
/// a new one appears. This function does not refresh the overlay: it reads
/// what the last `ecp` query wrote, and the reader rejects a fragment whose
/// source file changed since. `compute_search_hits` drops the hits that
/// leave unproven and starts the refresh.
///
/// Attaches only when the session holds dirty fragments built on exactly this
/// graph. A worktree with no session dir costs one `stat`; a clean one with
/// a session adds the `commits/` listing, one `session_meta.json` and one
/// `dirty_events` read, and builds no view; a session on another commit (the
/// hook falls back to the newest graph when HEAD is unindexed) reads the base.
/// No session id attaches nothing.
fn with_dirty_overlay(engine: Engine, index_dir: &Path, top: &Path, sid: Option<&str>) -> Engine {
    let (Some(repo_root), Some(sid)) = (index_dir.parent().and_then(Path::parent), sid) else {
        return engine;
    };
    if ecp_core::registry::validate_cache_component(sid).is_err() {
        return engine;
    }
    let name = crate::session::resolver::session_dir_name(sid, top);
    let session_dir = repo_root.join("sessions").join(&name);
    if !session_dir.is_dir() {
        return engine;
    }
    match crate::session::state::classify(repo_root, &name) {
        SessionState::AugmentedReference { l2_dirname, .. }
            if index_dir
                .file_name()
                .is_some_and(|n| n == l2_dirname.as_str()) =>
        {
            engine.with_overlay(session_dir, top.to_path_buf(), None)
        }
        _ => engine,
    }
}

/// Render hits as a legacy-style multi-line block. Each symbol gets a
/// header `name (file:line) [kind]` followed by optional `Called by:`
/// and `Calls:` lines drawn from the in-process 1-hop CSR expansion in
/// `compute_hits`; each lists distinct symbols, and a truncated line ends in
/// `+N more`. Empty caller / callee lists are skipped to keep the
/// per-hit footprint tight; the LLM reads the absence as "no callers
/// found within 1 hop" rather than asking ecp for a deeper trace.
pub fn format_hits(hits: &[Hit]) -> String {
    render_hits(hits, MAX_BYTES)
}

/// `format_hits` within `max_bytes`.
fn render_hits(hits: &[Hit], max_bytes: usize) -> String {
    let mut out = String::from(HITS_HEADER);
    for h in hits.iter().take(MAX_HITS) {
        let mut block = format!("  {} ({}:{}) [{}]\n", h.name, h.file, h.line, h.kind);
        push_neighbours(&mut block, "Called by", &h.callers, h.caller_count);
        push_neighbours(&mut block, "Calls", &h.callees, h.callee_count);
        if out.len() + block.len() > max_bytes {
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

/// One `label: a, b, c` line. `names` is capped at `HOP_EXPANSION_LIMIT`
/// and `total` is the real count, so the rest is named, not silently dropped.
fn push_neighbours(block: &mut String, label: &str, names: &[String], total: usize) {
    if names.is_empty() {
        return;
    }
    let _ = write!(block, "    {label}: {}", names.join(", "));
    let more = total.saturating_sub(names.len());
    if more > 0 {
        let _ = write!(block, ", +{more} more");
    }
    block.push('\n');
}

/// The directories, besides the envelope cwd, whose worktree this tool call
/// works in: a leading `cd` target of a Bash command, or the folder of the file
/// an edit tool writes. Each has its own session dir, so each needs its own
/// heartbeat.
pub(super) fn other_worktree_dirs(input: &HookInput) -> Vec<PathBuf> {
    match input.tool_name.as_str() {
        "Bash" => match bash_command(&input.tool_input).map(|cmd| cd_target(cmd, &input.cwd)) {
            Some(CdTarget::Dir(dir)) => vec![PathBuf::from(dir)],
            _ => Vec::new(),
        },
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ["file_path", "notebook_path"]
            .iter()
            .find_map(|key| input.tool_input.get(key).and_then(|v| v.as_str()))
            .and_then(|file| Path::new(file).parent())
            .filter(|dir| dir.is_absolute())
            .map(Path::to_path_buf)
            .into_iter()
            .collect(),
        _ => Vec::new(),
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
        let word_start = METACHARS.contains(&prev);
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
            // A newline after `&&`, `||` or `|` continues that list.
            (None, '\n')
                if matches!(
                    segments.last(),
                    Some((_, Some(Sep::And | Sep::Or | Sep::Pipe)))
                ) && cmd[start..comment_at.unwrap_or(i)].trim().is_empty() =>
            {
                start = i + 1;
                comment_at = None;
                continue;
            }
            (None, ';' | '\n') => Sep::Seq,
            // `2>&1`, `<&0`, `>|file` and `&>file` are redirects, not operators.
            (None, '&' | '|') if after_redirect => continue,
            (None, '&') if chars.peek().is_some_and(|&(_, n)| n == '>') => continue,
            // `|&` pipes stderr too.
            (None, '|') if chars.next_if(|&(_, n)| n == '&').is_some() => Sep::Pipe,
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
    /// a subshell or a background job.
    Unchanged,
    Dir(String),
    /// A cd may or may not run (`make || cd x`, `make && cd x; rg`), or its
    /// target cannot be known without expanding it (`cd -`, `$VAR`,
    /// backticks, `~user`, options, a bare `cd`), or a builtin other than cd
    /// moves the shell (`pushd`, `source`, ...), or a quote is left open.
    /// So does a cd in a `{ }` group or compound command that closes before
    /// the search, a cd in a loop, and a cd in the search's own subshell.
    Unknown,
}

const CWD_BUILTINS: [&str; 8] = [
    "pushd", "popd", "source", ".", "eval", "builtin", "command", "exec",
];

/// Git options that move the directory `git grep` searches.
const GIT_DIR_OPTIONS: [&str; 3] = ["-C", "--git-dir", "--work-tree"];

/// Environment variables that move the directory `git grep` searches.
const GIT_DIR_ENV: [&str; 2] = ["GIT_DIR=", "GIT_WORK_TREE="];

/// The characters after which a `#` starts a comment.
const METACHARS: [char; 10] = [' ', '\t', '\n', ';', '&', '|', '(', ')', '<', '>'];

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
    if !cmd.contains("cd")
        && !CWD_BUILTINS.iter().any(|b| cmd.contains(b))
        && !GIT_DIR_OPTIONS
            .iter()
            .chain(&GIT_DIR_ENV)
            .any(|o| cmd.contains(o))
    {
        return CdTarget::Unchanged;
    }
    let joined = join_continuations(cmd);
    let cmd = joined.as_ref();
    let Some(segments) = shell_segments(cmd) else {
        return CdTarget::Unknown;
    };
    let cdpath = cdpath || cmd.contains("CDPATH");
    let words: Vec<Vec<String>> = segments.iter().map(|&(s, _)| shell_words(s)).collect();
    // `extract_from_shell` takes the first `rg` / `grep` word that
    // `runs_search` accepts, so the scan stops at the same word.
    let search = words
        .iter()
        .position(|w| (0..w.len()).any(|j| runs_search(w, j)));
    // `git -C dir grep` and `GIT_WORK_TREE=dir git grep` search `dir`, which
    // the cd walk does not track.
    let git_moves_grep = |k: usize| {
        let segment = &words[k];
        let Some(j) = (0..segment.len()).find(|&j| runs_search(segment, j)) else {
            return false;
        };
        segment[..j]
            .iter()
            .rposition(|w| w == "git")
            .is_some_and(|g| {
                segment[g + 1..j]
                    .iter()
                    .any(|w| GIT_DIR_OPTIONS.iter().any(|o| w.starts_with(o)))
                    || words[..=k]
                        .iter()
                        .flatten()
                        .any(|w| GIT_DIR_ENV.iter().any(|e| w.starts_with(e)))
            })
    };
    if search.is_some_and(git_moves_grep) {
        return CdTarget::Unknown;
    }
    let marks: Vec<_> = segments.iter().map(|&(s, _)| group_marks(s)).collect();
    let commands: Vec<(Vec<&str>, usize)> = words.iter().map(|w| command_words(w)).collect();
    let compounds: Vec<(Vec<bool>, usize)> = commands
        .iter()
        .map(|(cmd_words, after_prefix)| compound_marks(cmd_words, *after_prefix))
        .collect();
    let depth_after: Vec<usize> = marks
        .iter()
        .zip(&compounds)
        .scan(0usize, |depth, (&((po, pc), (bo, bc)), (opens, closes))| {
            *depth = (*depth + po + bo + opens.len()).saturating_sub(pc + bc + closes);
            Some(*depth)
        })
        .collect();
    let moves = |w: &&str| *w == "cd" || CWD_BUILTINS.contains(w);
    // A search in a function body runs wherever the function is called,
    // which the walk does not follow (`f() { rg …; }; cd x && f`).
    let defines_function = |(c, _): &(Vec<&str>, usize)| match c[..] {
        ["function", ..] | [_, "()", ..] => true,
        [first, ..] => first.len() > 2 && first.ends_with("()"),
        [] => false,
    };
    if search.is_some_and(|k| commands[..=k].iter().any(defines_function)) {
        return CdTarget::Unknown;
    }
    // A `case` pattern's `)` reads as a subshell close, so the depth below
    // is wrong inside one.
    let has_case = commands.iter().any(|(c, p)| c.get(*p) == Some(&"case"));
    if has_case
        && search.is_some_and(|k| {
            commands[..k]
                .iter()
                .any(|(c, _)| c.iter().any(|w| moves(&w.trim_start_matches('('))))
        })
    {
        return CdTarget::Unknown;
    }
    // An and-or list ending in `&` runs as a background job: a cd reaches the
    // search only when every list around the cd that ends before the search
    // ends in `;`. A `;` or `&` deeper than the cd (`cd x && (a & wait)`)
    // ends no list around it.
    let other_job = |i: usize| {
        let mut level = Some(depth_after[i]);
        for (j, &(_, sep)) in segments
            .iter()
            .enumerate()
            .take(search.unwrap_or(0))
            .skip(i)
        {
            let Some(l) = level else { break };
            if matches!(sep, None | Some(Sep::Seq | Sep::Background)) && depth_after[j] <= l {
                if sep == Some(Sep::Background) {
                    return true;
                }
                level = depth_after[j].checked_sub(1);
            }
        }
        false
    };
    // A later `.` alone is mostly a path argument (`git add .`).
    let moves_shell = |cmd_words: &[&str]| {
        cmd_words
            .first()
            .is_some_and(|w| moves(&w.trim_start_matches('(')))
            || cmd_words.iter().skip(1).any(|w| *w != "." && moves(w))
    };
    // `x=$(cd y && rg …)` opens a subshell on the word that holds the cd.
    let opens_moving_subshell = |words: &[String]| {
        words
            .iter()
            .any(|w| w.rsplit_once('(').is_some_and(|(_, c)| moves(&c)))
    };
    // A cd before `else` / `elif` belongs to the branch that is not taken.
    let switches_branch = |words: &[String]| {
        words
            .iter()
            .take_while(|w| PREFIX_WORDS.contains(&w.as_str()))
            .any(|w| w == "else" || w == "elif")
    };
    // A loop around the search runs a later cd before its next pass.
    let moves_after_search =
        search.is_some_and(|k| words[k + 1..].iter().flatten().any(|w| moves(&w.as_str())));
    let mut current: Option<PathBuf> = None;
    let mut before = Some(Sep::Seq);
    let mut subshell = 0usize;
    // A cd inside a subshell that is still open moves a search in it.
    let mut subshell_moves = false;
    // Open `{ }` groups and compound commands, `true` for a loop or a `case`.
    // A group may be a function body, a skipped operand or a pipeline's
    // subshell, so a cd inside one holds only until it closes; a loop runs
    // its cd again, and a `case` arm may not be the search's arm.
    let mut groups: Vec<bool> = Vec::new();
    let mut cd_depth = 0;
    // After `cd x ||` succeeds, its `||` operand does not run. The operand
    // ends at the next `;`, `&&` or `&` outside any group or compound it opens
    // (`a || b && c` is `(a || b) && c`).
    let mut skipping: Option<usize> = None;
    for (i, (&(segment, after), words)) in segments.iter().zip(&words).enumerate() {
        if Some(i) == search {
            if subshell > 0 && subshell_moves
                // `until grep -q x f; do cd ..; done` searches in its condition.
                || (groups.contains(&true) || compounds[i].0.contains(&true)) && moves_after_search
                || cd_depth > 0 && cd_depth >= groups.len() && switches_branch(words)
            {
                return CdTarget::Unknown;
            }
            break;
        }
        let ((paren_opens, paren_closes), (brace_opens, brace_closes)) = marks[i];
        if let Some(depth) = skipping.as_mut() {
            let (compound_opens, compound_closes) = &compounds[i];
            *depth = (*depth + paren_opens + brace_opens + compound_opens.len())
                .saturating_sub(paren_closes + brace_closes + compound_closes);
            if *depth == 0 && !matches!(after, Some(Sep::Or | Sep::Pipe)) {
                skipping = None;
            }
            before = after;
            continue;
        }
        let (cmd_words, after_prefix) = (&commands[i].0, commands[i].1);
        let starts_subshell = cmd_words.first().is_some_and(|w| w.starts_with('('));
        // `x=$( cd y && pwd )` runs the cd in the substitution.
        let opens_in_assignment = words
            .iter()
            .take_while(|w| is_assignment(w))
            .any(|w| w.contains('('));
        let in_subshell = subshell > 0 || starts_subshell || opens_in_assignment;
        // Backticks hold a command the split does not see.
        if words
            .iter()
            .any(|w| w.rsplit_once('`').is_some_and(|(_, c)| moves(&c)))
        {
            return CdTarget::Unknown;
        }
        subshell_moves |= in_subshell && moves_shell(cmd_words) || opens_moving_subshell(words);
        subshell = (subshell + paren_opens).saturating_sub(paren_closes);
        if subshell == 0 {
            subshell_moves = false;
        }
        // `cat | { cd x; rg; }`: the pipe runs the group, and the cd starts
        // the group's own list.
        let opens_group = words
            .iter()
            .find(|w| !is_assignment(w))
            .is_some_and(|w| w == "{");
        let before_cmd = if opens_group { Some(Sep::Seq) } else { before };
        let own_shell = !in_subshell
            && !other_job(i)
            && before_cmd != Some(Sep::Pipe)
            && after != Some(Sep::Pipe);
        let (compound_opens, compound_closes) = &compounds[i];
        groups.truncate(groups.len().saturating_sub(brace_closes + compound_closes));
        if groups.len() < cd_depth
            || cd_depth > 0 && cd_depth >= groups.len() && switches_branch(words)
        {
            return CdTarget::Unknown;
        }
        groups.extend(compound_opens);
        groups.extend(std::iter::repeat_n(false, brace_opens));
        if own_shell
            && (after_prefix > 0 && cmd_words.get(after_prefix).is_some_and(moves)
                || cmd_words.iter().skip(1).any(|w| *w != "." && moves(w)))
        {
            return CdTarget::Unknown;
        }
        match cmd_words.first().copied() {
            // `command -v rg` looks a name up and runs nothing.
            Some("command") if matches!(cmd_words.get(1), Some(&("-v" | "-V"))) => {}
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
                // `make && cd x && rg` searches only where the cd ran, but
                // `false && cd x && true; rg` searches the old directory.
                let surely_runs = match before_cmd {
                    Some(Sep::Seq | Sep::Background) | None => true,
                    Some(Sep::And) => search.is_some_and(|k| {
                        segments[i..k]
                            .iter()
                            .all(|&(_, s)| matches!(s, Some(Sep::And | Sep::Pipe)))
                    }),
                    _ => false,
                };
                let [_, arg] = cmd_words[..] else {
                    return CdTarget::Unknown;
                };
                if !surely_runs || groups.contains(&true) {
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
                        cd_depth = groups.len();
                        if after == Some(Sep::Or) {
                            skipping = Some(0);
                        }
                    }
                    // A directory missing now may be made before the cd
                    // runs (`mkdir x && cd x`, `git worktree add x`); if it
                    // is not, `cd x || …` runs an operand the hook does not
                    // model.
                    Some(None) => return CdTarget::Unknown,
                }
            }
            _ => {}
        }
        before = after;
    }
    match current {
        None => CdTarget::Unchanged,
        // `dunce` drops the `\\?\` prefix wherever the plain form names the
        // same path; a long or UNC path keeps it.
        Some(dir) => dunce::canonicalize(dir)
            .ok()
            .and_then(|dir| dir.into_os_string().into_string().ok())
            .map_or(CdTarget::Unknown, CdTarget::Dir),
    }
}

/// A segment's words after its assignments and an opening `{`, with the
/// count of leading prefix words (`!`, `then`, `time -p`). A prefix the
/// split does not model may still run the command in this shell
/// (`! cd x`, `time . env.sh`, `then cd x`).
fn command_words(words: &[String]) -> (Vec<&str>, usize) {
    let cmd_words: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .skip_while(|w| is_assignment(w))
        .skip_while(|w| *w == "{")
        .collect();
    let mut after_prefix = 0;
    // `time -p` takes options before its command word.
    let mut time_options = false;
    for &word in &cmd_words {
        if PREFIX_WORDS.contains(&word) {
            time_options = word == "time";
        } else if !(time_options && word.starts_with('-')) {
            break;
        }
        after_prefix += 1;
    }
    (cmd_words, after_prefix)
}

/// The compound commands a segment opens, `true` for a loop or a `case`,
/// and how many it closes (`fi`, `done`, `esac`).
fn compound_marks(cmd_words: &[&str], after_prefix: usize) -> (Vec<bool>, usize) {
    let mut opens: Vec<bool> = cmd_words[..after_prefix]
        .iter()
        .filter(|w| matches!(**w, "if" | "while" | "until"))
        .map(|w| *w != "if")
        .collect();
    if let Some(&("for" | "select" | "case")) = cmd_words.get(after_prefix) {
        opens.push(true);
    }
    let closes = usize::from(matches!(cmd_words.first(), Some(&("fi" | "done" | "esac"))));
    (opens, closes)
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
        let word_start = out.is_empty() || out.ends_with(METACHARS);
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            // A comment's trailing backslash continues nothing.
            (None, '#') if word_start => {
                out.push(c);
                while let Some(next) = chars.next_if(|&n| n != '\n') {
                    out.push(next);
                }
                continue;
            }
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
                // A metacharacter ends the word too: `{ a; }>log` closes.
                let ends_word = |n: &u8| {
                    n.is_ascii_whitespace()
                        || c == b'}' && matches!(n, b'>' | b'<' | b'&' | b'|' | b';' | b')')
                };
                let word = (i == 0 || bytes[i - 1].is_ascii_whitespace())
                    && bytes.get(i + 1).is_none_or(ends_word);
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
    if raw.next() != Some("cd") {
        return false;
    }
    // `cd 2>/dev/null ~/x`: a redirection before the argument is no argument.
    while let Some(word) = raw.next() {
        let op = word.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
        if !op.starts_with(['<', '>']) {
            return word.starts_with('~');
        }
        // `2> log` puts the target in the next word.
        if op.trim_start_matches(['<', '>', '&', '|']).is_empty() {
            raw.next();
        }
    }
    false
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
/// `grep "pub struct HookInput"` is one word, not three. An unquoted
/// redirection (`2>&1`, `&>log`, `>/dev/null`, `< in`) is dropped with its
/// target, as the command never sees it: `cd x>/dev/null` names `x`.
fn shell_words(cmd: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut drop_next = false;
    let mut chars = cmd.chars().peekable();
    let mut finish = |current: &mut String, drop_next: &mut bool| {
        let word = std::mem::take(current);
        if !std::mem::take(drop_next) {
            words.push(word);
        }
    };
    while let Some(c) = chars.next() {
        match c {
            // `<(cmd)` / `>(cmd)` is a process substitution, not a redirect.
            '<' | '>' if chars.peek() != Some(&'(') => {
                // `2>` and `&>` name the redirected stream.
                let stream = current.chars().all(|d| d.is_ascii_digit()) || current == "&";
                if in_word && stream {
                    current.clear();
                } else if in_word {
                    finish(&mut current, &mut drop_next);
                }
                in_word = false;
                while chars
                    .next_if(|&n| matches!(n, '<' | '>' | '&' | '|'))
                    .is_some()
                {}
                drop_next = true;
            }
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
                    finish(&mut current, &mut drop_next);
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
        finish(&mut current, &mut drop_next);
    }
    words
}

/// Whether `words[i]` runs `rg` / `grep`, rather than naming it to a
/// lookup (`command -v rg`, `which grep`, `type -p rg`), listing files
/// (`rg --files | xargs grep …`) or printing its version or help.
fn runs_search(words: &[String], i: usize) -> bool {
    if !matches!(words[i].as_str(), "rg" | "grep") {
        return false;
    }
    // `shell_words` keeps a separator glued to its word (`grep -V;`).
    for word in &words[i + 1..] {
        let bare = word.trim_end_matches(['|', '&', ';']);
        match bare {
            "--version" | "-V" | "--help" => return false,
            "--files" | "--type-list" if words[i] == "rg" => return false,
            _ => {}
        }
        if bare.len() < word.len() {
            break;
        }
    }
    let flags = words[..i]
        .iter()
        .rev()
        .take_while(|w| w.starts_with('-'))
        .count();
    // `cd grep` names a directory; `command -v rg grep` names two tools.
    let names = |w: Option<&str>| {
        matches!(
            w,
            Some("which" | "type" | "hash" | "whereis" | "cd" | "pushd")
        )
    };
    let mut head = words[..i]
        .iter()
        .map(String::as_str)
        .skip_while(|w| is_assignment(w) || *w == "{" || PREFIX_WORDS.contains(w));
    let looks_up = match head.next() {
        Some("command") => head
            .take_while(|w| w.starts_with('-'))
            .any(|w| w.contains(['v', 'V'])),
        first => names(first),
    };
    !looks_up
        && match words[..i - flags].last().map(String::as_str) {
            Some("command") => flags == 0,
            last => !names(last),
        }
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
        "--color",
        "--colour",
        "--max-count",
        "--context",
        "--after-context",
        "--before-context",
        "-j",
        "--threads",
        "--max-depth",
        "-T",
        "--type-not",
    ];
    let mut found_cmd = false;
    let mut skip_next = false;
    // `-e PATTERN` names the pattern explicitly for both grep and rg; the
    // word after it is the answer, not a flag value to step over.
    let mut next_is_pattern = false;
    // `cd_target_with_home` reads the same segment, so the term and the
    // directory belong to one search.
    let words = shell_segments(cmd)?
        .into_iter()
        .map(|(segment, _)| shell_words(segment))
        .find(|w| (0..w.len()).any(|i| runs_search(w, i)))?;
    for (i, word) in words.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if !found_cmd {
            found_cmd = runs_search(&words, i);
            continue;
        }
        // `--regexp=PATTERN` and `-ePATTERN` carry the pattern in the word.
        let glued = word
            .strip_prefix("--regexp=")
            .or_else(|| word.strip_prefix("-e").filter(|p| !p.is_empty()));
        if let Some(pattern) = glued.filter(|_| !next_is_pattern) {
            if pattern.len() >= 3 {
                return Some(pattern.to_string());
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
            return Some(word.clone());
        }
        next_is_pattern = false;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::CdTarget::{Dir, Unchanged, Unknown};
    use super::{
        cd_target_with_home, extract_from_shell, keep_recorded_hits, omitted_line, search_terms,
        shell_segments, spawn_overlay_refresh_with, CdTarget, RefreshMarkers, Sep, HITS_HEADER,
        MAX_BYTES,
    };
    use crate::commands::find::{Hit, ScoreSource};
    use std::cell::{Cell, RefCell};
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    fn hit_in(file: &str, name: &str) -> Hit {
        Hit {
            repo: None,
            score: 1.0,
            score_source: ScoreSource::Bm25,
            kind: "Function".to_string(),
            file: file.to_string(),
            language: "Rust".to_string(),
            line: 1,
            name: name.to_string(),
            signature: String::new(),
            caller_count: 0,
            callers: vec![],
            callee_count: 0,
            callees: vec![],
            category: ecp_core::graph::FileCategory::Source,
        }
    }

    /// Contract: the 2 KB cap holds with the omitted line in, and the line
    /// names two files then counts the rest, so a long list never grows it.
    #[test]
    fn test_omitted_line_many_files_names_two_and_counts_rest() {
        let files: Vec<String> = ["a.rs", "b.rs", "c.rs", "d.rs"].map(String::from).into();
        let line = omitted_line(&files, 5);
        assert_eq!(
            line,
            "  omitted: 5 hit(s) in a.rs, b.rs, +2 more (file changed since ecp last read it)\n"
        );
        assert!(HITS_HEADER.len() + line.len() < MAX_BYTES);
        assert_eq!(omitted_line(&[], 0), "");
    }

    /// Contract: the proof runs once per distinct file, a hit with no file
    /// names no file:line and stays, and only unproven files are named.
    #[test]
    fn test_keep_recorded_hits_proves_each_file_once_and_keeps_fileless_hits() {
        let hits = vec![
            hit_in("a.rs", "one"),
            hit_in("b.rs", "two"),
            hit_in("a.rs", "three"),
            hit_in("", "route"),
        ];
        let calls = RefCell::new(Vec::new());
        let (kept, dropped) = keep_recorded_hits(hits, |file| {
            calls.borrow_mut().push(file.to_string());
            file == "b.rs"
        });
        assert_eq!(*calls.borrow(), ["a.rs", "b.rs"]);
        assert_eq!(
            kept.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            ["two", "route"]
        );
        assert_eq!((dropped.hits, dropped.files), (2, vec!["a.rs".to_string()]));
    }

    fn launch(
        markers: &RefreshMarkers,
        top: &Path,
        newest_change: Option<SystemTime>,
    ) -> (bool, Option<(Vec<String>, String)>) {
        let seen = RefCell::new(None);
        let started =
            spawn_overlay_refresh_with(markers, top, "sid-1", "pat", newest_change, |job| {
                *seen.borrow_mut() = Some((
                    job.args.iter().map(|a| a.to_string()).collect(),
                    job.env
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<String>(),
                ));
                true
            });
        (started, seen.into_inner())
    }

    /// Contract: a refresh that finished after the file's last change does
    /// not run again, so a file no refresh can prove (an LFS file) does not
    /// start a job on every hook call.
    #[test]
    fn test_maybe_spawn_overlay_refresh_marker_newer_than_dropped_file_does_not_spawn() {
        let tmp = tempfile::tempdir().unwrap();
        let markers = RefreshMarkers::under(tmp.path(), "sid-1");
        std::fs::create_dir_all(&markers.dir).unwrap();
        std::fs::write(&markers.failed, b"").unwrap();
        let before_marker = SystemTime::now() - Duration::from_secs(60);
        assert!(!launch(&markers, tmp.path(), Some(before_marker)).0);
        assert!(!launch(&markers, tmp.path(), None).0);
    }

    #[test]
    fn test_maybe_spawn_overlay_refresh_no_marker_or_older_marker_spawns_find_with_session() {
        let tmp = tempfile::tempdir().unwrap();
        let markers = RefreshMarkers::under(tmp.path(), "sid-1");
        let (started, seen) = launch(&markers, tmp.path(), None);
        assert!(started);
        let (args, env) = seen.unwrap();
        assert_eq!(args[0], "find");
        assert_eq!(args.last().unwrap(), "pat");
        assert_eq!(env, "ECP_SESSION_ID=sid-1");

        std::fs::write(&markers.complete, b"").unwrap();
        let after_marker = SystemTime::now() + Duration::from_secs(60);
        assert!(launch(&markers, tmp.path(), Some(after_marker)).0);
    }

    #[test]
    fn test_maybe_spawn_overlay_refresh_dir_uncreatable_does_not_spawn() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".hook-refresh"), b"file").unwrap();
        let markers = RefreshMarkers::under(tmp.path(), "sid-1");
        let called = Cell::new(false);
        let started =
            spawn_overlay_refresh_with(&markers, tmp.path(), "sid-1", "pat", None, |_| {
                called.set(true);
                true
            });
        assert!(!started && !called.get());
    }

    fn canon(p: &Path) -> String {
        dunce::canonicalize(p)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()
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
        let mut cmds = vec![
            format!("cd \"{d}\" && grep -rn CacheKeyQueue app"),
            format!("cd '{d}' && grep -rn CacheKeyQueue app"),
        ];
        // Unquoted, a Windows path's `\` separators are escapes to bash too.
        if cfg!(unix) {
            cmds.push(format!("cd {} && rg Foo", d.replace(' ', "\\ ")));
        }
        for cmd in cmds {
            assert_eq!(target(&cmd, other.path()), dir(&path), "{cmd}");
        }
        // A relative path carries no separator bash would eat, so the escaped
        // space holds on every platform.
        let relative = "cd 業主\\ 原始碼/outsourcing-backend-CN && rg Foo";
        assert_eq!(target(relative, t.path()), dir(&path), "{relative}");
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
        assert_eq!(
            go("cd 2>/dev/null ~/x && grep Foo", Some(h.path())),
            dir(&h.path().join("x"))
        );
        assert_eq!(
            go("cd 2> /dev/null ~/x && grep Foo", Some(h.path())),
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
    fn test_cd_target_nonexistent_dir_returns_unknown() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            target("cd /no/such/dir_zz && grep Foo .", t.path()),
            Unknown
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
            "cd x; time -p . env.sh; rg Foo",
            "cd x; cd /u?r; rg Foo",
            "cd x; set -P; cd ..; rg Foo",
            "cd x; set -o physical; cd ..; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), Unknown, "{cmd:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_cd_target_parent_of_symlink_is_logical() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("real/inner")).unwrap();
        std::os::unix::fs::symlink(t.path().join("real/inner"), t.path().join("link")).unwrap();
        assert_eq!(target("cd link; cd ..; rg Foo", t.path()), dir(t.path()));
        assert_eq!(target("cd link/..; rg Foo", t.path()), dir(t.path()));
        assert_eq!(target("cd missing/..; rg Foo", t.path()), Unknown);
        assert_eq!(target("cd link/../missing; rg Foo", t.path()), Unknown);
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
        std::fs::create_dir(t.path().join("y")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "cd x && \\\nrg Foo",
            "cd \\\nx; rg Foo",
            "cd x && time git diff -- .; rg Foo",
            "cd y\n# cd z && \\\ncd ../x\nrg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd:?}");
        }
        // Single quotes keep the backslash-newline: the cd names `x\<nl>y`,
        // which does not exist, not `xy`, which does.
        std::fs::create_dir(t.path().join("x/xy")).unwrap();
        assert_eq!(target("cd x; cd 'x\\\ny'; rg Foo", t.path()), Unknown);
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
    fn test_cd_target_cd_that_may_not_reach_the_search_is_unknown() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        for cmd in [
            "(cd x && rg Foo) | head",
            "false && cd x && true; rg Foo",
            "false &&\ncd x; rg Foo",
            "false && # note\n\ncd x; rg Foo",
            "false || \ncd x; rg Foo",
            "false && { cd x; }; rg Foo",
            "{ cd x; } | cat; rg Foo",
            "f() {\ncd x\n}\nrg Foo",
            "if false; then true; cd x; fi; rg Foo",
            "while true; do true; cd x; rg Foo; done",
            "for d in a b; do rg Foo; cd x; done",
            "mkdir -p new && cd new && rg -n Foo src",
            "git worktree add ../wt && cd ../wt && rg Foo",
            "cd x &&\nuntil grep -q '^w' Cargo.toml 2>&1; do\n  cd .. || exit 1\ndone",
            "if test -f /dev/null; then\n  cd x\nelse rg Foo .\nfi",
            "if false; then\n  cd x\nelse\n  true; rg Foo .\nfi",
            "case a in\nb)\n  cd x;;\na) rg Foo;;\nesac",
            "m=$(cd x && rg -l Foo .)",
            "m=`cd x && rg -l Foo .`",
            "git -C x grep -n Foo",
            "GIT_WORK_TREE=/x git grep -n Foo",
            "export GIT_DIR=/x/.git; git grep Foo",
            "scan() {\n  rg -n Foo src | head\n}\ncd x && scan",
            "function scan { rg -n Foo src; }; cd x && scan",
            "scan () { rg Foo; }; cd x; scan",
            "cd x && case a in\n  a) make ;;\n  b) make test ;;\nesac &\nrg -n Foo src\nwait",
            "cd x && git --work-tree=y grep Foo",
        ] {
            assert_eq!(target(cmd, t.path()), Unknown, "{cmd:?}");
        }
    }

    #[test]
    fn test_cd_target_cd_every_path_to_the_search_runs_keeps_cd() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        std::fs::create_dir(t.path().join("grep")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "make && cd x && rg Foo",
            "make && cd x && cat f | rg Foo",
            "cd x &&\nrg Foo",
            "cat f | { cd x; rg Foo; }",
            "if true; then true; cd x; rg Foo; fi",
            "command -v rg && cd x && rg Foo .",
            "which grep >/dev/null; cd x; grep -rn Foo .",
            "command -v rg grep git\ncd x && rg -n Foo src",
            "which rg grep; cd x && rg -n Foo src",
            "cd grep && cd ../x && rg -n main src",
            "rg --version &&\ncd x &&\nrg -n Foo src",
            "cd x>/dev/null &&\nrg -n Foo src",
            "cd x 2>/dev/null && rg Foo",
            "cd x || if test -d y; then\n  cd y\nfi\nrg -n Foo src",
            "cd x && rg -n 'fn main()' src",
            "cd 2>/dev/null x && rg Foo",
            "cd x &>/dev/null; rg Foo",
            "cd x &&\n  (git status >/dev/null &\n   git diff --stat >/dev/null & wait)\nrg -n Foo src",
            "cd x && {\n  rg -n -m 1 Foo a\n  rg -n -m 1 Bar a\n} & wait",
            "cd x && git status --short |& head\nrg -n Foo src",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd:?}");
        }
        for cmd in [
            "cd x &&\nif test -f a; then\n  git diff --stat\nfi &\nrg -n Foo a\nwait",
            "cd x && { git status --short; }>/dev/null 2>&1 &\nrg -n Foo src\nwait",
            "cd x && if test -d .; then pwd; fi>/dev/null 2>&1 &\nrg -n Foo src\nwait",
            "cd x && { git status --short; } >/dev/null 2>&1 &\nrg -n Foo src\nwait",
            "make &&\nrg Foo",
            "m=$(cd x && pwd); rg Foo .",
            "git grep -n Foo",
            "root=$( cd x && pwd -P )\nrg -n Foo src",
            "rg -C 3 Foo",
            "git -C x status; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), Unchanged, "{cmd:?}");
        }
    }

    #[test]
    fn test_extract_from_shell_lookup_of_rg_is_not_the_search() {
        for cmd in [
            "rg --color never -n -m 2 HookInput src",
            "cd grep && rg -n HookInput src",
            "rg --version && cd /tmp && rg -n HookInput src",
            "rg 2>/dev/null HookInput src",
            "rg -n --regexp=HookInput src/a.rs",
            "grep -rn -eHookInput src",
            "grep -rn HookInput>/tmp/out .",
            "cd /a\nrg -n -m 1 HookInput src\ncd /b\nrg --files -g x.rs | xargs grep -n other_name",
            "grep -V; grep -rn HookInput .",
            "command -v rg grep git\ncd /tmp && rg -n -m 1 HookInput src",
            "rg --files -g '**/x.rs' | xargs grep -n -m 2 HookInput",
            "command -v rg && cd /tmp && rg HookInput .",
            "which grep; grep -rn HookInput .",
            "type -p rg >/dev/null && rg HookInput",
        ] {
            assert_eq!(
                extract_from_shell(cmd).as_deref(),
                Some("HookInput"),
                "{cmd}"
            );
        }
        assert_eq!(
            extract_from_shell("command rg HookInput").as_deref(),
            Some("HookInput")
        );
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
