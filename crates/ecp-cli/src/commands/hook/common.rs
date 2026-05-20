//! Shared utilities for Claude Code hook event handlers:
//! stdin JSON envelope parsing, hookSpecificOutput emission, registry-
//! aware index dir resolution, hook-local `.ecp/` state dir creation,
//! and shell-quote stripping shared between PreToolUse (pattern
//! extraction) and PostToolUse (git mutation detection).

use ecp_core::registry::{resolve_home_ecp, RegistryFile};
use ecp_core::EcpError;
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
pub fn read_stdin_envelope() -> Result<HookInput, EcpError> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(EcpError::Io)?;
    if buf.trim().is_empty() {
        return Ok(HookInput {
            cwd: String::new(),
            tool_name: String::new(),
            tool_input: Value::Null,
            tool_output: Value::Null,
        });
    }
    serde_json::from_str(&buf)
        .map_err(|e| EcpError::InvalidArgument(format!("hook stdin parse: {e}")))
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

/// Hook-local state dir at `<cwd>/.ecp/`. Used only for marker files
/// (`.rebuild-complete`, `.rebuild-failed`), the rebuild log, and the
/// `.analyze.lock` — things tied to *this* worktree, not the shared
/// index in `~/.ecp/<repo>__<hash>/commits/<sha>/`.
///
/// Read-side: returns `Some` iff cwd is absolute AND `<cwd>/.ecp/`
/// already exists. Hooks must not block tool execution on missing
/// dirs — callers translate `None` into a silent no-op.
pub fn ecp_state_dir(cwd: &str) -> Option<PathBuf> {
    let path = Path::new(cwd);
    if !path.is_absolute() {
        return None;
    }
    let candidate = path.join(".ecp");
    candidate.exists().then_some(candidate)
}

/// Write-side variant: returns `Some(<cwd>/.ecp/)` and creates the
/// directory if absent. Used by PostToolUse (which needs to drop a
/// `.rebuild-complete` / `.rebuild-failed` marker even on the very
/// first run, before any other tool has touched the dir).
pub fn ecp_state_dir_ensure(cwd: &str) -> Option<PathBuf> {
    let path = Path::new(cwd);
    if !path.is_absolute() {
        return None;
    }
    let candidate = path.join(".ecp");
    fs::create_dir_all(&candidate).ok()?;
    Some(candidate)
}

/// Registry-aware index dir resolution. Reads `~/.ecp/registry.json`,
/// finds the `RepoAlias` whose `common_dir` matches cwd's git common-dir,
/// then resolves the commit dir for the current branch's HEAD SHA.
///
/// Branch-affinity primary: resolves HEAD SHA and looks up its commit dir
/// so hook on branch A always loads branch A's graph even when branch B
/// was indexed more recently (restores the invariant from 47596ff).
///
/// Falls back to the most-recently-built commit dir when the current
/// branch hasn't been indexed yet — same behavior as the original
/// `find_by_cwd(branch_hint)` fallback.
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
    let home_ecp = resolve_home_ecp();
    let registry_path = home_ecp.join("registry.json");
    let registry = RegistryFile::read_or_empty(&registry_path).ok()?;
    let alias = crate::repo_selector::find_by_path(&registry, cwd)?;
    let commits_dir = home_ecp.join(&alias.dir_name).join("commits");

    // Branch-affinity primary: HEAD SHA → exact commit dir.
    if let Some(head) = crate::graph_path::head_sha_bytes(path) {
        let idx = CommitIndex::scan(&commits_dir).ok()?;
        if let Some(dir) = idx.find(&head) {
            return Some(commits_dir.join(dir));
        }
    }

    // Fallback: most-recently-built commit dir (current branch not yet indexed).
    crate::commit_lookup::find_latest_by_mtime(&commits_dir)
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

/// Drain this session's peer inbox, render to payload, truncate inbox.
/// Returns `Some(payload_string)` if there was something to inject, `None` otherwise.
/// Honors `ECP_REPO_ROOT_OVERRIDE` for tests.
fn default_repo_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let repo_dir = crate::repo_identity::repo_dir_name_for_cwd(&cwd).ok()?;
    Some(resolve_home_ecp().join(repo_dir))
}

pub fn drain_and_render_peer_payload() -> Option<String> {
    let me = crate::session::resolver::resolve_session_id(None);
    let repo_root: PathBuf = std::env::var("ECP_REPO_ROOT_OVERRIDE")
        .map(PathBuf::from)
        .ok()
        .or_else(default_repo_root)?;
    let session_dir = repo_root.join("sessions").join(&me);
    let inbox = session_dir.join("inbox.jsonl");

    // Fast path: skip meta read + inbox open entirely when inbox is absent or empty.
    // Covers the vast majority of PreToolUse fires with a single stat call.
    match std::fs::metadata(&inbox) {
        Err(_) => return None,
        Ok(m) if m.len() == 0 => return None,
        Ok(_) => {}
    }

    let meta_path = session_dir.join("meta.json");
    let mut meta = ecp_core::session::SessionMeta::read(&meta_path).ok()?;

    let (entries, _new_offset) =
        ecp_core::peer::inbox::drain(&inbox, meta.last_drained_offset).ok()?;
    if entries.is_empty() {
        return None;
    }
    let payload = crate::peer::render::render_payload(&entries);
    if payload.is_empty() {
        return None;
    }

    let _ = ecp_core::peer::inbox::truncate_inbox(&inbox);
    meta.last_drained_offset = 0;
    let _ = ecp_core::session::SessionMeta::write_atomic(&meta_path, &meta);
    Some(payload)
}
