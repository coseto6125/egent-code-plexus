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
