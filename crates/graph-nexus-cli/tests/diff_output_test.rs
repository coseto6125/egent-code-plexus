//! Verify text / toon output formats for `gnx diff`.

use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn head_sha() -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap()
        .stdout;
    String::from_utf8_lossy(&out).trim().to_string()
}

#[test]
fn diff_text_output_has_section_header() {
    let sha = head_sha();
    let output = Command::new(gnx_bin())
        .args([
            "diff",
            "--section",
            "bindings",
            "--baseline",
            &sha,
            "--format",
            "text",
        ])
        .output()
        .expect("run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Bindings") || stdout.contains("bindings"),
        "text output must label bindings section: {stdout}"
    );
}

#[test]
fn diff_toon_output_parses() {
    let sha = head_sha();
    let output = Command::new(gnx_bin())
        .args([
            "diff",
            "--section",
            "bindings",
            "--baseline",
            &sha,
            "--format",
            "toon",
        ])
        .output()
        .expect("run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Toon's key:value structure: at minimum has baseline/sections labels.
    assert!(
        stdout.contains("baseline") || stdout.contains("sections"),
        "toon output should mention baseline/sections: {stdout}"
    );
}

#[test]
fn diff_json_output_parses_as_valid_json() {
    let sha = head_sha();
    let output = Command::new(gnx_bin())
        .args([
            "diff",
            "--section",
            "bindings",
            "--baseline",
            &sha,
            "--format",
            "json",
        ])
        .output()
        .expect("run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}; stdout: {stdout}"));
    assert!(parsed["baseline"].is_object(), "expected baseline object");
    assert!(parsed["sections"].is_object(), "expected sections object");
}
