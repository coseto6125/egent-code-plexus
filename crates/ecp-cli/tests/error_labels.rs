//! A refusal reaches the user under its own words, not under a label that
//! names a different failure. Before this fix `ecp admin index` printed its
//! home-directory refusal as
//! `output encode error: git rev-parse HEAD: refusing to index ...`, and
//! `ecp find` printed it as `invalid argument: Error preparing index ...`:
//! causes that did not happen, in front of the one that did.
//!
//! Stands for: each absent text means "no false cause label"; the present
//! text "refusing to index" means "the real cause still reaches the user".

use std::process::Command;

/// Runs `ecp <args>` in a fresh commitless `$HOME`, which ecp refuses, and
/// returns the exit status and the combined stderr and stdout.
fn run_in_commitless_home(args: &[&str]) -> (bool, String) {
    let home = tempfile::tempdir().unwrap();
    let ecp_home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("a.py"), "def foo():\n    pass\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args(args)
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("ECP_HOME", ecp_home.path())
        .env("ECP_SKIP_BG_REBUILD", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // Keeps a repository above the temp dir from vouching for the home.
        .env("GIT_CEILING_DIRECTORIES", home.path())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .unwrap();
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    (out.status.success(), text)
}

#[test]
fn test_admin_index_refusal_prints_no_output_encode_label() {
    let (ok, text) = run_in_commitless_home(&["admin", "index", "--force", "--repo", "."]);
    assert!(!ok, "a commitless home is refused: {text}");
    assert!(text.contains("refusing to index"), "{text}");
    assert!(!text.contains("output encode error"), "{text}");
    assert!(!text.contains("git rev-parse HEAD"), "{text}");
}

#[test]
fn test_find_refusal_prints_no_invalid_argument_label() {
    let (ok, text) = run_in_commitless_home(&["find", "foo"]);
    assert!(!ok, "a commitless home is refused: {text}");
    assert!(text.contains("refusing to index"), "{text}");
    assert!(!text.contains("invalid argument"), "{text}");
}
