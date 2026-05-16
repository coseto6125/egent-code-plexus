use graph_nexus_cli::session::resolver::resolve_session_id;

const ENV_KEY: &str = "CLAUDE_CODE_SESSION_ID";

#[test]
fn explicit_takes_precedence_over_env() {
    std::env::set_var(ENV_KEY, "from-env");
    let id = resolve_session_id(Some("from-cli"));
    assert_eq!(id, "from-cli");
    std::env::remove_var(ENV_KEY);
}

#[test]
fn env_used_when_no_explicit() {
    std::env::set_var(ENV_KEY, "test-session-xyz");
    let id = resolve_session_id(None);
    assert_eq!(id, "test-session-xyz");
    std::env::remove_var(ENV_KEY);
}

#[test]
fn empty_explicit_falls_through_to_env() {
    std::env::set_var(ENV_KEY, "env-wins");
    let id = resolve_session_id(Some(""));
    assert_eq!(id, "env-wins");
    std::env::remove_var(ENV_KEY);
}

#[test]
fn empty_env_falls_through_to_pid() {
    std::env::remove_var(ENV_KEY);
    let id = resolve_session_id(None);
    assert!(id.starts_with("cli-"), "got: {id}");
    assert_eq!(
        id.len(),
        "cli-".len() + 8,
        "expected 4-byte hex (8 chars) suffix, got: {id}"
    );
}

#[test]
fn pid_fallback_format_is_hex8() {
    std::env::remove_var(ENV_KEY);
    let id = resolve_session_id(None);
    let suffix = id.strip_prefix("cli-").expect("must start with cli-");
    assert!(
        suffix.chars().all(|c| c.is_ascii_hexdigit()),
        "suffix must be hex: {suffix}"
    );
}
