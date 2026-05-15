//! Verify `gnx diff --section contracts --baseline <ref>` returns
//! contract changes between two refs.

use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

#[test]
fn diff_contracts_head_vs_head_empty() {
    let head_sha = {
        let out = Command::new("git").args(["rev-parse", "HEAD"]).output().unwrap().stdout;
        String::from_utf8_lossy(&out).trim().to_string()
    };
    let output = Command::new(gnx_bin())
        .args(["diff", "--section", "contracts", "--baseline", &head_sha, "--format", "json"])
        .output()
        .expect("run gnx diff contracts");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let contracts = &parsed["sections"]["contracts"];
    for key in ["added", "removed", "modified"] {
        assert!(
            contracts[key].as_array().expect("array").is_empty(),
            "{key} should be empty: {contracts:?}"
        );
    }
}
