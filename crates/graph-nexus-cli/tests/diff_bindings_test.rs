//! Verify `gnx diff --section bindings --baseline <ref>` returns
//! resolver decision changes between two refs.

use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

#[test]
fn diff_bindings_against_head_yields_empty() {
    // Diff HEAD vs HEAD: no resolver decisions changed.
    let head_sha = {
        let out = Command::new("git").args(["rev-parse", "HEAD"]).output().unwrap().stdout;
        String::from_utf8_lossy(&out).trim().to_string()
    };
    let output = Command::new(gnx_bin())
        .args(["diff", "--section", "bindings", "--baseline", &head_sha,
               "--format", "json"])
        .output()
        .expect("run gnx diff bindings");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}; stdout was: {stdout}"));
    let bindings = &parsed["sections"]["bindings"];
    for key in ["new_resolutions", "tier_changes", "target_changes", "removed"] {
        let arr = bindings[key].as_array()
            .unwrap_or_else(|| panic!("missing {key}"));
        assert!(arr.is_empty(), "{key} should be empty for HEAD vs HEAD; got {arr:?}");
    }
}
