//! Integration test for `gnx admin config` — covers (a) non-TTY fallback path
//! and (b) round-tripping through the TOML config file.
//!
//! The TUI itself (ratatui main loop + keystroke handling) is not exercised
//! here — driving a ratatui app through stdin is fragile and the user-
//! review-via-dry-run pattern doesn't apply. Coverage of the wiring +
//! load/save is what we get from this test.

use graph_nexus_core::config::{config_path, load, save, Config};
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

#[test]
fn non_tty_invocation_prints_help_message_and_exits_zero() {
    // stdout is piped → not a TTY → must hit the fallback path.
    let repo = tempfile::tempdir().unwrap();
    let out = Command::new(gnx_bin())
        .args(["admin", "config", "--repo", repo.path().to_str().unwrap()])
        .output()
        .expect("gnx admin config failed to spawn");
    assert!(
        out.status.success(),
        "non-tty path should exit 0, got {:?}; stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("requires an interactive terminal"),
        "missing TTY hint in stderr; got: {stderr}",
    );
    assert!(
        stderr.contains("config.toml"),
        "should point at config path; got: {stderr}",
    );
}

#[test]
fn round_trip_load_save_preserves_all_fields() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();

    let mut cfg = Config::default();
    cfg.embedding.model = "nomic-embed-text".into();
    cfg.embedding.endpoint = "http://localhost:11434/v1".into();
    cfg.embedding.api_key = "sk-test".into();
    cfg.embedding.batch_size = 64;
    cfg.output.default_format = "json".into();
    cfg.confidence.high_trust_threshold = 0.65;

    save(root, &cfg).expect("save");
    assert!(config_path(root).exists());

    let loaded = load(root).expect("load");
    assert_eq!(loaded, cfg);
}

#[test]
fn malformed_toml_returns_error_with_path_in_message() {
    let repo = tempfile::tempdir().unwrap();
    let cfg_path = config_path(repo.path());
    std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
    std::fs::write(&cfg_path, "this is not valid toml = =\n").unwrap();

    let err = load(repo.path()).expect_err("load should reject garbage TOML");
    assert!(err.contains("parse"), "expected 'parse' in error: {err}");
    assert!(err.contains("config.toml"), "expected path in error: {err}");
}
