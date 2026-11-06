//! Resolve the active LLM session-id for L1 dir naming.
//!
//! Precedence: explicit CLI flag > env ECP_SESSION_ID > host session env >
//! pid-based fallback. Hooks pass session_id via env (already populated by
//! Claude Code / MCP transport); Codex exposes a stable thread id. Direct CLI
//! invocations without any of the above get a per-process fallback id derived
//! from PID + nanosecond timestamp.

use std::path::Path;
use std::sync::OnceLock;
use xxhash_rust::xxh3::Xxh3;

const SESSION_ENV_KEYS: [&str; 5] = [
    "ECP_SESSION_ID",
    "GEMINI_CLI_SESSION_ID",
    "CODEX_SESSION_ID",
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
];

pub fn resolve_session_id(explicit: Option<&str>) -> String {
    if let Some(s) = explicit {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    for key in SESSION_ENV_KEYS {
        if let Ok(s) = std::env::var(key) {
            if !s.is_empty() {
                return s;
            }
        }
    }
    fallback_id().to_string()
}

/// Session id for a hook process: the environment wins (so an explicit
/// `ECP_SESSION_ID` still applies), then the non-empty `session_id` of the hook
/// input, then the per-process fallback. A hook process may not carry the host's
/// session env var while its JSON input always names the session.
pub fn resolve_hook_session_id(input_session_id: &str) -> String {
    hook_session_id(input_session_id).unwrap_or_else(|| fallback_id().to_string())
}

/// The session a hook names, in `resolve_hook_session_id`'s order, or `None`
/// when no env var and no input id names one: the per-process fallback names
/// no overlay dir, so a caller that writes under the id must not use it.
pub fn hook_session_id(input_session_id: &str) -> Option<String> {
    for key in SESSION_ENV_KEYS {
        if let Ok(s) = std::env::var(key) {
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    (!input_session_id.is_empty()).then(|| input_session_id.to_string())
}

// The parser lives in ecp-core so `alive_peers` shares it; this module keeps
// the name every session-dir caller already imports.
pub use ecp_core::peer::registry::{split_session_dir_name, SESSION_DIR_SEP};

/// The dir name under `sessions/` for session `sid` working in `worktree`:
/// `<sid>~<h8>`, where `h8` hashes the worktree's own gitdir. Two worktrees of
/// one repo that run under one session id each get their own overlay, manifest
/// and `base_sha`. The gitdir is the same for a subdirectory or a symlinked
/// alias of the worktree; a path outside git keys by its canonical path. One
/// cached `git_layout` lookup and one short hash.
pub fn session_dir_name(sid: &str, worktree: &Path) -> String {
    // The on-disk layout, never the env-checked one: `GIT_CEILING_DIRECTORIES`
    // or `GIT_DIR` in one process and not in another must not give one
    // worktree two keys.
    let key = crate::git_cache::git_layout_unchecked(worktree)
        .map_or_else(|| fallback_worktree_key(worktree), |(_, gitdir, _)| gitdir);
    let key = dunce::simplified(&key).to_string_lossy().into_owned();
    format!(
        "{sid}{SESSION_DIR_SEP}{}",
        crate::repo_identity::short_hash_hex8(key.as_bytes())
    )
}

/// True when session dir names `a` and `b` both carry a worktree key and the
/// keys differ. Sessions of one worktree share its files on disk, so equal
/// content there still means two agents edit one file.
pub fn in_other_worktrees(a: &str, b: &str) -> bool {
    match (split_session_dir_name(a), split_session_dir_name(b)) {
        (Some((_, ka)), Some((_, kb))) => ka != kb,
        _ => false,
    }
}

/// Key for a path whose `.git` git would not open (an empty `.git` dir): the
/// nearest ancestor that holds a `.git` entry,
/// so a subdirectory keys like its worktree root. Without any `.git` ancestor
/// the canonical path itself.
fn fallback_worktree_key(worktree: &Path) -> std::path::PathBuf {
    let canonical = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    canonical
        .ancestors()
        .find(|a| a.join(".git").symlink_metadata().is_ok())
        .map_or_else(|| canonical.clone(), Path::to_path_buf)
}

/// `session_dir_name` for the session the process env names and the
/// process's current directory.
pub fn own_session_dir_name() -> std::io::Result<String> {
    Ok(session_dir_name(
        &resolve_session_id(None),
        &std::env::current_dir()?,
    ))
}

/// Resolve the team-visible agent name for this session, if any.
/// `ECP_AGENT_NAME` (explicit contract) wins over `CLAUDE_AGENT_NAME`
/// (harness-provided). Whitespace-only values are treated as unset.
pub fn resolve_agent_name() -> Option<String> {
    ["ECP_AGENT_NAME", "CLAUDE_AGENT_NAME"]
        .iter()
        .find_map(|k| match std::env::var(k) {
            Ok(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            _ => None,
        })
}

/// Per-PROCESS fallback id. Must be computed exactly once: the L1 overlay
/// writer (`apply_l1_overlay_updates`) and reader (engine construction) each
/// resolve the session dir independently within one CLI invocation, so a
/// per-call timestamp would send fragments to one session dir and read
/// another — making the overlay invisible precisely when no agent host
/// provides a session id (bare CLI, CI).
fn fallback_id() -> &'static str {
    static FALLBACK: OnceLock<String> = OnceLock::new();
    FALLBACK.get_or_init(|| {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut h = Xxh3::new();
        h.update(&pid.to_le_bytes());
        h.update(&nanos.to_le_bytes());
        format!("cli-{:016x}", h.digest())
    })
}

#[cfg(test)]
mod tests {
    use super::{fallback_id, session_dir_name};

    #[test]
    fn test_session_dir_name_subdir_of_ungit_resolved_worktree_keys_like_its_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let deep = root.join("sub/deep");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        assert_eq!(session_dir_name("s", &root), session_dir_name("s", &deep));
    }

    #[test]
    fn test_session_dir_name_without_git_ancestor_keys_by_canonical_path() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        assert_ne!(session_dir_name("s", &a), session_dir_name("s", &b));
    }

    #[test]
    fn fallback_id_is_stable_within_a_process() {
        assert_eq!(
            fallback_id(),
            fallback_id(),
            "overlay writer and reader resolve the session dir independently; \
             an unstable fallback splits them across two dirs"
        );
    }
}
