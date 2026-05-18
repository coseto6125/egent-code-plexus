//! Resolve the active LLM session-id for L1 dir naming.
//!
//! Precedence: explicit CLI flag > env CLAUDE_CODE_SESSION_ID > pid-based fallback.
//! Hooks pass session_id via env (already populated by Claude Code / MCP transport).
//! Direct CLI invocations without any of the above get a stable per-process
//! fallback id derived from PID + nanosecond timestamp.

use xxhash_rust::xxh3::Xxh3;

pub fn resolve_session_id(explicit: Option<&str>) -> String {
    if let Some(s) = explicit {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    if let Ok(s) = std::env::var("CLAUDE_CODE_SESSION_ID") {
        if !s.is_empty() {
            return s;
        }
    }
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = Xxh3::new();
    h.update(&pid.to_le_bytes());
    h.update(&nanos.to_le_bytes());
    format!("cli-{:016x}", h.digest())
}
