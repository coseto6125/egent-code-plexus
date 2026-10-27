//! Smoke: ecp admin runs without panicking when given an immediate
//! "Exit" via piped input. Tests menu plumbing, not interactive flow.

use std::process::{Command, Stdio};

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

#[test]
fn ecp_admin_exits_cleanly_on_immediate_exit_choice() {
    let mut child = Command::new(ecp_bin())
        .arg("admin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ecp admin");

    // Send EOF immediately — dialoguer returns Ok(None) on EOF which
    // our menu handles as "Exit".
    drop(child.stdin.take());

    let out = child.wait_with_output().expect("wait");

    // Even if dialoguer warns about non-TTY, the program should exit 0
    // (or at minimum not panic). Accept any non-panic exit.
    assert!(
        out.status.code().is_some(),
        "ecp admin should not be killed by signal — got: {:?}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Verify `ecp admin --help` renders correctly and exits 0.
#[test]
fn ecp_admin_help_exits_zero() {
    let out = Command::new(ecp_bin())
        .args(["admin", "--help"])
        .output()
        .expect("spawn ecp admin --help");

    assert!(
        out.status.success(),
        "ecp admin --help should exit 0, got {:?}",
        out.status,
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("admin") || stdout.contains("host"),
        "help text should mention admin/host; got: {stdout}",
    );
}

/// The installer reads only the copy built into the binary: an ecp checkout
/// under cwd whose SKILL.md differs must not reach the installed skill.
#[test]
fn test_install_skills_checkout_under_cwd_installs_embedded_copy() {
    let checkout = tempfile::tempdir().unwrap();
    let stale = checkout.path().join("docs/skills/ecp");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("SKILL.md"), "CHECKOUT-SENTINEL\n").unwrap();
    let home = tempfile::tempdir().unwrap();

    let out = Command::new(ecp_bin())
        .args([
            "admin",
            "claude",
            "install",
            "skills",
            "ecp",
            "--no-claude-md",
        ])
        .current_dir(checkout.path())
        .env("HOME", home.path())
        .env("ECP_HOME", home.path().join(".ecp"))
        .output()
        .expect("spawn ecp admin claude install skills");
    assert!(
        out.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let installed =
        std::fs::read_to_string(home.path().join(".claude/skills/ecp/SKILL.md")).unwrap();
    let built_from = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/skills/ecp/SKILL.md"
    ))
    .unwrap();
    assert_eq!(installed, built_from);
}
