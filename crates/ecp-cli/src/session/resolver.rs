//! Resolve the active LLM session-id for L1 dir naming.
//!
//! Precedence: explicit CLI flag > env ECP_SESSION_ID > host session env >
//! pid-based fallback. Hooks pass session_id via env (already populated by
//! Claude Code / MCP transport); Codex exposes a stable thread id. Direct CLI
//! invocations without any of the above get a per-process fallback id derived
//! from PID + nanosecond timestamp.

use std::sync::OnceLock;
use xxhash_rust::xxh3::Xxh3;

pub fn resolve_session_id(explicit: Option<&str>) -> String {
    if let Some(s) = explicit {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    for key in [
        "ECP_SESSION_ID",
        "GEMINI_CLI_SESSION_ID",
        "CODEX_SESSION_ID",
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
    ] {
        if let Ok(s) = std::env::var(key) {
            if !s.is_empty() {
                return s;
            }
        }
    }
    fallback_id().to_string()
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
    use super::fallback_id;

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
