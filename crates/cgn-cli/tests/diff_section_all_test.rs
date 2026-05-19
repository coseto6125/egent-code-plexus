//! `--section all` must produce the same JSON envelope as
//! `--section bindings,routes,contracts`.

use std::process::Command;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

fn head_sha() -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap()
        .stdout;
    String::from_utf8_lossy(&out).trim().to_string()
}

fn run_diff_json(sections: &str) -> serde_json::Value {
    let sha = head_sha();
    let out = Command::new(cgn_bin())
        .args([
            "diff",
            "--section",
            sections,
            "--baseline",
            &sha,
            "--format",
            "json",
        ])
        .output()
        .expect("run cgn diff");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn section_all_equals_explicit_three() {
    let a = run_diff_json("all");
    let b = run_diff_json("bindings,routes,contracts");
    let a_sections = a["sections"].as_object().unwrap();
    let b_sections = b["sections"].as_object().unwrap();
    for key in ["bindings", "routes", "contracts"] {
        assert_eq!(
            a_sections.get(key),
            b_sections.get(key),
            "section {key} must be identical between --section all and explicit list"
        );
    }
}
