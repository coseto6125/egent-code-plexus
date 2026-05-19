use graph_nexus_cli::session::resolver::resolve_session_id;
use std::sync::Mutex;

const ENV_KEYS: &[&str] = &[
    "GNX_SESSION_ID",
    "CODEX_SESSION_ID",
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
];
const CLAUDE_ENV_KEY: &str = "CLAUDE_CODE_SESSION_ID";
const CODEX_ENV_KEY: &str = "CODEX_THREAD_ID";
const GENERIC_ENV_KEY: &str = "GNX_SESSION_ID";

/// All tests in this file mutate session env vars. cargo's
/// default parallel test runner interleaves them in the same process,
/// so without serialization one test's `set_var` is observed by
/// another's `resolve_session_id`. Use the same `static Mutex<()>`
/// pattern as `tests/force_rebuild_test.rs` (`HOME_LOCK`) — no extra
/// crate dep, just hold the guard for the whole test body.
static SESSION_ENV_LOCK: Mutex<()> = Mutex::new(());

fn clear_session_env() {
    for key in ENV_KEYS {
        std::env::remove_var(key);
    }
}

#[test]
fn explicit_takes_precedence_over_env() {
    let _guard = SESSION_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    clear_session_env();
    std::env::set_var(CLAUDE_ENV_KEY, "from-env");
    let id = resolve_session_id(Some("from-cli"));
    assert_eq!(id, "from-cli");
    clear_session_env();
}

#[test]
fn claude_env_used_when_no_explicit() {
    let _guard = SESSION_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    clear_session_env();
    std::env::set_var(CLAUDE_ENV_KEY, "test-session-xyz");
    let id = resolve_session_id(None);
    assert_eq!(id, "test-session-xyz");
    clear_session_env();
}

#[test]
fn codex_thread_env_used_when_no_explicit() {
    let _guard = SESSION_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    clear_session_env();
    std::env::set_var(CODEX_ENV_KEY, "codex-thread-xyz");
    let id = resolve_session_id(None);
    assert_eq!(id, "codex-thread-xyz");
    clear_session_env();
}

#[test]
fn generic_env_takes_precedence_over_host_env() {
    let _guard = SESSION_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    clear_session_env();
    std::env::set_var(GENERIC_ENV_KEY, "generic-session");
    std::env::set_var(CODEX_ENV_KEY, "codex-thread-xyz");
    std::env::set_var(CLAUDE_ENV_KEY, "claude-session");
    let id = resolve_session_id(None);
    assert_eq!(id, "generic-session");
    clear_session_env();
}

#[test]
fn empty_explicit_falls_through_to_env() {
    let _guard = SESSION_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    clear_session_env();
    std::env::set_var(CLAUDE_ENV_KEY, "env-wins");
    let id = resolve_session_id(Some(""));
    assert_eq!(id, "env-wins");
    clear_session_env();
}

#[test]
fn empty_env_falls_through_to_pid() {
    let _guard = SESSION_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    clear_session_env();
    for key in ENV_KEYS {
        std::env::set_var(key, "");
    }
    let id = resolve_session_id(None);
    assert!(id.starts_with("cli-"), "got: {id}");
    assert_eq!(
        id.len(),
        "cli-".len() + 16,
        "expected 8-byte hex (16 chars) suffix, got: {id}"
    );
    clear_session_env();
}

#[test]
fn pid_fallback_format_is_hex16() {
    let _guard = SESSION_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    clear_session_env();
    let id = resolve_session_id(None);
    let suffix = id.strip_prefix("cli-").expect("must start with cli-");
    assert_eq!(
        suffix.len(),
        16,
        "expected 8-byte hex (16 chars) suffix, got: {id}"
    );
    assert!(
        suffix.chars().all(|c| c.is_ascii_hexdigit()),
        "suffix must be hex: {suffix}"
    );
    clear_session_env();
}
