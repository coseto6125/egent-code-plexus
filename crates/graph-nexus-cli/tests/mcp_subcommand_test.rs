//! Drives `gnx mcp tools` and asserts the output enumerates the 8
//! expected tools.

use std::process::Command;

#[test]
fn gnx_mcp_tools_lists_eight_tools() {
    let bin = env!("CARGO_BIN_EXE_gnx");
    let out = Command::new(bin).args(["mcp", "tools"]).output().expect("spawn");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    for expected in [
        "gnx_context", "gnx_impact", "gnx_query", "gnx_detect_changes",
        "gnx_rename", "gnx_route_map", "gnx_shape_check", "gnx_multi_query",
    ] {
        assert!(stdout.contains(expected), "missing {expected} in:\n{stdout}");
    }
}
