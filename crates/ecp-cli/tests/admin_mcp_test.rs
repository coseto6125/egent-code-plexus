use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

#[test]
fn admin_mcp_tools_lists_tools() {
    let output = Command::new(ecp_bin())
        .args(["admin", "mcp", "tools"])
        .output()
        .expect("run ecp admin mcp tools");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("inspect"),
        "expected `inspect` tool in list, got: {stdout}"
    );
}

#[test]
fn top_level_mcp_no_longer_visible() {
    let output = Command::new(ecp_bin())
        .args(["--help"])
        .output()
        .expect("run ecp --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("\n  mcp "),
        "mcp must NOT appear as top-level command in --help, got: {stdout}"
    );
}

#[test]
fn admin_mcp_appears_under_admin_help() {
    let output = Command::new(ecp_bin())
        .args(["admin", "--help"])
        .output()
        .expect("run ecp admin --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("mcp"),
        "expected `mcp` subcommand under admin, got: {stdout}"
    );
}

#[test]
fn admin_mcp_tools_json_format() {
    let output = Command::new(ecp_bin())
        .args(["admin", "mcp", "tools", "--format", "json"])
        .output()
        .expect("run ecp admin mcp tools --format json");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("output must be valid JSON");
    assert!(
        parsed.is_array() || parsed.get("tools").is_some(),
        "expected JSON array or {{tools: [...]}} object, got: {parsed}"
    );
}
