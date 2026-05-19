use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

#[test]
fn admin_verify_resolver_help_lists_required_args() {
    let output = Command::new(gnx_bin())
        .args(["admin", "verify-resolver", "--help"])
        .output()
        .expect("run cgn admin verify-resolver --help");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--oracle"), "expected --oracle arg in help: {stdout}");
    assert!(stdout.contains("--cgn"), "expected --cgn arg in help: {stdout}");
}

#[test]
fn top_level_verify_resolver_no_longer_dispatches() {
    let output = Command::new(gnx_bin())
        .args(["verify-resolver", "--help"])
        .output()
        .expect("run cgn verify-resolver --help");
    // Should fail because top-level command was removed
    assert!(
        !output.status.success() || !String::from_utf8_lossy(&output.stdout).contains("oracle"),
        "verify-resolver must not be a top-level command"
    );
}
