use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

#[test]
fn dev_verify_resolver_help_lists_required_args() {
    let output = Command::new(ecp_bin())
        .args(["dev", "verify-resolver", "--help"])
        .output()
        .expect("run ecp dev verify-resolver --help");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--oracle"),
        "expected --oracle arg in help: {stdout}"
    );
    assert!(
        stdout.contains("--ecp"),
        "expected --ecp arg in help: {stdout}"
    );
}

#[test]
fn admin_verify_resolver_alias_removed() {
    let output = Command::new(ecp_bin())
        .args(["admin", "verify-resolver", "--help"])
        .output()
        .expect("run ecp admin verify-resolver --help");
    // After alias removal, this subcommand no longer exists under `admin`.
    assert!(
        !output.status.success(),
        "admin verify-resolver should no longer be a recognized subcommand"
    );
}

#[test]
fn top_level_verify_resolver_no_longer_dispatches() {
    let output = Command::new(ecp_bin())
        .args(["verify-resolver", "--help"])
        .output()
        .expect("run ecp verify-resolver --help");
    // Should fail because top-level command was removed
    assert!(
        !output.status.success() || !String::from_utf8_lossy(&output.stdout).contains("oracle"),
        "verify-resolver must not be a top-level command"
    );
}
