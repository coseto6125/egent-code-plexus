//! A refusal reaches the user under its own words, not under a label that
//! names a different failure. Before this fix `ecp admin index` printed its
//! home-directory refusal as
//! `output encode error: git rev-parse HEAD: refusing to index ...`: two
//! causes that did not happen, in front of the one that did.
//!
//! Stands for: each absent text ("output encode error", "git rev-parse HEAD")
//! means "no false cause label"; the present text "refusing to index" means
//! "the real cause still reaches the user".

use std::process::Command;

#[test]
fn test_admin_index_refusal_prints_no_output_encode_label() {
    let home = tempfile::tempdir().unwrap();
    let ecp_home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("a.py"), "def foo():\n    pass\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args(["admin", "index", "--force", "--repo", "."])
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("ECP_HOME", ecp_home.path())
        .env("ECP_SKIP_BG_REBUILD", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
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

    assert!(!out.status.success(), "a non-git home is refused: {text}");
    assert!(text.contains("refusing to index"), "{text}");
    assert!(!text.contains("output encode error"), "{text}");
    assert!(!text.contains("git rev-parse HEAD"), "{text}");
}
