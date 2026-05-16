//! Shared utilities for Claude Code hook event handlers:
//! stdin JSON envelope parsing, hookSpecificOutput emission, registry-
//! aware index dir resolution, hook-local `.gnx/` state dir creation,
//! and shell-quote stripping shared between PreToolUse (pattern
//! extraction) and PostToolUse (git mutation detection).

use graph_nexus_core::registry::{resolve_home_gnx, RegistryFile};
use graph_nexus_core::GnxError;
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Parsed Claude Code stdin envelope. Only the fields the hook
/// handlers actually consume are extracted; everything else is
/// silently ignored so we tolerate future protocol additions.
#[derive(Debug, Deserialize)]
pub struct HookInput {
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Value,
    #[serde(default)]
    pub tool_output: Value,
}

/// Read stdin to EOF and decode into `HookInput`. An empty stdin
/// resolves to an all-default `HookInput` rather than an error — the
/// hook may legitimately be invoked with no envelope (e.g. by the
/// SessionStart event when Claude Code hasn't surfaced any context).
pub fn read_stdin_envelope() -> Result<HookInput, GnxError> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(GnxError::Io)?;
    if buf.trim().is_empty() {
        return Ok(HookInput {
            cwd: String::new(),
            tool_name: String::new(),
            tool_input: Value::Null,
            tool_output: Value::Null,
        });
    }
    serde_json::from_str(&buf)
        .map_err(|e| GnxError::InvalidArgument(format!("hook stdin parse: {e}")))
}

/// Emit `{"hookSpecificOutput": {"hookEventName": ..., "additionalContext": ...}}`
/// to stdout. Caller passes the canonical Claude Code event name
/// (CamelCase: "PreToolUse", "UserPromptSubmit", "PostToolUse",
/// "SessionStart").
pub fn emit_additional_context(event: &str, context: &str) {
    let payload = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": context,
        }
    });
    println!("{}", payload);
}

/// Hook-local state dir at `<cwd>/.gnx/`. Used only for marker files
/// (`.rebuild-complete`, `.rebuild-failed`), the rebuild log, and the
/// `.analyze.lock` — things tied to *this* worktree, not the shared
/// index in `~/.gnx/<repo>/<branch>/`.
///
/// Read-side: returns `Some` iff cwd is absolute AND `<cwd>/.gnx/`
/// already exists. Hooks must not block tool execution on missing
/// dirs — callers translate `None` into a silent no-op.
pub fn gnx_state_dir(cwd: &str) -> Option<PathBuf> {
    let path = Path::new(cwd);
    if !path.is_absolute() {
        return None;
    }
    let candidate = path.join(".gnx");
    candidate.exists().then_some(candidate)
}

/// Write-side variant: returns `Some(<cwd>/.gnx/)` and creates the
/// directory if absent. Used by PostToolUse (which needs to drop a
/// `.rebuild-complete` / `.rebuild-failed` marker even on the very
/// first run, before any other tool has touched the dir).
pub fn gnx_state_dir_ensure(cwd: &str) -> Option<PathBuf> {
    let path = Path::new(cwd);
    if !path.is_absolute() {
        return None;
    }
    let candidate = path.join(".gnx");
    fs::create_dir_all(&candidate).ok()?;
    Some(candidate)
}

/// Registry-aware index dir resolution. Reads `~/.gnx/registry.json`,
/// finds the `RepoAlias` whose `common_dir` matches cwd's git common-dir,
/// then scans the repo's commits dir for the most recent graph.bin.
///
/// Returns `None` when:
///   - cwd is not absolute (defensive: shell envs occasionally arrive empty)
///   - the registry file doesn't exist or can't be parsed
///   - no `RepoAlias` covers cwd (worktree never registered)
///   - the matched repo has zero built commits
pub fn lookup_index_dir(cwd: &str) -> Option<PathBuf> {
    use crate::commit_lookup::CommitIndex;

    let path = Path::new(cwd);
    if !path.is_absolute() {
        return None;
    }
    let home_gnx = resolve_home_gnx();
    let registry_path = home_gnx.join("registry.json");
    let registry = RegistryFile::read_or_empty(&registry_path).ok()?;
    let alias = crate::repo_selector::find_by_path(&registry, cwd)?;
    let commits_dir = home_gnx.join(&alias.dir_name).join("commits");
    let idx = CommitIndex::scan(&commits_dir).ok()?;
    if idx.is_empty() {
        return None;
    }
    // Pick the commit dir with the most recent graph.bin mtime.
    std::fs::read_dir(&commits_dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let g = e.path().join("graph.bin");
            let mtime = std::fs::metadata(&g).ok()?.modified().ok()?;
            Some((mtime, g.parent()?.to_path_buf()))
        })
        .max_by_key(|(mtime, _)| *mtime)
        .map(|(_, dir)| dir)
}

/// Resolve the current branch by reading `.git/HEAD` directly instead
/// of spawning `git rev-parse`. Hooks fire on every Claude Code event;
/// a fork+exec per event adds a few ms on local FS and visibly stalls
/// on NFS / WSL2 paths. Reading the HEAD ref file is a single open+read
/// on any FS, with no subprocess overhead.
///
/// Handles three layouts:
///   - regular repo: `<cwd>/.git` is a directory; read `<cwd>/.git/HEAD`
///   - worktree: `<cwd>/.git` is a file containing `gitdir: <abs path>`
///   - cwd is a subdir: walk parents until `.git` is found
///
/// Returns `None` when not in a git work tree, on detached HEAD, or on
/// any read error — callers fall back to "most recent indexed".
fn current_git_branch(cwd: &Path) -> Option<String> {
    let toplevel = find_git_toplevel(cwd)?;
    let head_path = resolve_head_path(&toplevel)?;
    let content = fs::read_to_string(head_path).ok()?;
    let line = content.lines().next()?;
    line.strip_prefix("ref: refs/heads/").map(str::to_string)
}

fn find_git_toplevel(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        current = current.parent()?.to_path_buf();
    }
}

fn resolve_head_path(toplevel: &Path) -> Option<PathBuf> {
    let dotgit = toplevel.join(".git");
    let meta = fs::metadata(&dotgit).ok()?;
    if meta.is_dir() {
        return Some(dotgit.join("HEAD"));
    }
    // `.git` is a file (worktree layout):
    //   `gitdir: /path/to/main-repo/.git/worktrees/<name>`
    let content = fs::read_to_string(&dotgit).ok()?;
    let gitdir = content.lines().next()?.strip_prefix("gitdir: ")?;
    Some(PathBuf::from(gitdir).join("HEAD"))
}

/// Remove the contents of single- and double-quoted segments from a
/// shell command so subsequent regex matchers don't trip on literal
/// substrings (e.g. `echo "git commit"` must not look like an actual
/// git invocation). Command substitution (`$(…)`, backticks) is
/// intentionally NOT stripped — commands inside DO execute, so they
/// should still be inspected.
pub fn strip_shell_quotes(cmd: &str) -> String {
    let bytes = cmd.as_bytes();
    let mut out = String::with_capacity(cmd.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\'' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'\'' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }
        if c == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    break;
                }
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }
        out.push(c as char);
        i += 1;
    }
    out
}
