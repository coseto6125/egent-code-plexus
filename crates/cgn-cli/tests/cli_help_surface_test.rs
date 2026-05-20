use std::process::Command;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

#[test]
fn top_level_help_contains_shape_check() {
    let output = Command::new(cgn_bin())
        .args(["--help"])
        .output()
        .expect("run cgn --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("shape_check") || stdout.contains("shape-check"),
        "expected shape_check in top-level --help, got: {stdout}"
    );
}

#[test]
fn top_level_help_excludes_admin_only_commands() {
    let output = Command::new(cgn_bin())
        .args(["--help"])
        .output()
        .expect("run cgn --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for hidden in ["verify-resolver", "verify_resolver"] {
        assert!(
            !stdout.contains(hidden),
            "{hidden} must not appear in top-level --help, got: {stdout}"
        );
    }
}

#[test]
fn top_level_help_contains_diff() {
    let output = Command::new(cgn_bin())
        .args(["--help"])
        .output()
        .expect("run cgn --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("diff"),
        "expected `diff` in top-level --help, got: {stdout}"
    );
}

#[test]
fn admin_help_contains_mcp_and_verify_resolver() {
    let output = Command::new(cgn_bin())
        .args(["admin", "--help"])
        .output()
        .expect("run cgn admin --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mcp"), "admin --help missing mcp: {stdout}");
    assert!(
        stdout.contains("codex"),
        "admin --help missing codex: {stdout}"
    );
    assert!(
        stdout.contains("verify-resolver"),
        "admin --help missing verify-resolver: {stdout}"
    );
}
