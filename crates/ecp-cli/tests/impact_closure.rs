//! Exercise closure references through the indexed CLI graph and baseline diff.
use serde_json::Value;
use std::path::Path;
use std::process::Command;

const SOURCE: &str = "export function target() {}\n\
export function enclosing() {\n  register(() => { target(); });\n}\n\
export function caller() { enclosing(); }\n";

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit(repo: &Path) {
    git(repo, &["add", "-A"]);
    git(
        repo,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "fixture",
        ],
    );
}

fn ecp(repo: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args(args)
        .current_dir(repo)
        .env("ECP_HOME", repo.join(".ecp"))
        .env("ECP_NO_TELEMETRY", "1")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ecp {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn index(repo: &Path) {
    ecp(repo, &["admin", "index", "--force", "--repo", "."]);
}

fn fixture() -> tempfile::TempDir {
    fixture_source(SOURCE)
}

fn fixture_source(source: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), ".ecp/\n").unwrap();
    std::fs::write(repo.join("lib.ts"), source).unwrap();
    commit(repo);
    index(repo);
    tmp
}

fn impact(repo: &Path, extra: &[&str]) -> Value {
    let mut args = vec![
        "impact",
        "--repo",
        ".",
        "--format",
        "json",
        "--exclude-tests",
    ];
    args.extend_from_slice(extra);
    let stdout = ecp(repo, &args);
    serde_json::from_str(&stdout[stdout.find('{').expect("JSON output")..]).unwrap()
}

fn baseline(repo: &Path, source: &str) -> Value {
    std::fs::write(repo.join("lib.ts"), source).unwrap();
    commit(repo);
    index(repo);
    impact(repo, &["--baseline", "HEAD~1"])
}

#[test]
fn test_impact_closure_reaches_named_functions_in_both_directions() {
    let tmp = fixture();
    for (target, direction, expected) in [
        ("target", "upstream", "enclosing"),
        ("enclosing", "downstream", "target"),
    ] {
        let value = impact(
            tmp.path(),
            &["--target", target, "--direction", direction, "--depth", "3"],
        );
        assert!(
            value["impact"]
                .as_array()
                .unwrap()
                .iter()
                .any(|node| node["name"] == expected),
            "{direction} must reach {expected}: {value}"
        );
    }
}

#[test]
fn test_impact_baseline_header_comment_has_no_changed_symbols_or_impact() {
    let tmp = fixture();
    let value = baseline(tmp.path(), &format!("// header\n{SOURCE}"));
    assert!(
        value["changed_symbols"].as_array().unwrap().is_empty(),
        "position-only closure move must not change symbols: {value}"
    );
    assert!(
        value["impact_by_symbol"].as_array().unwrap().is_empty(),
        "position-only closure move must not inflate impact: {value}"
    );
}

#[test]
fn test_impact_baseline_closure_body_edit_is_reported() {
    let tmp = fixture();
    let value = baseline(
        tmp.path(),
        &SOURCE.replace("{ target(); }", "{ target(); target(); }"),
    );
    assert!(
        value["changed_symbols"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |node| node["name"].as_str().unwrap().starts_with("<anonymous:")
                    && node["change_type"] == "modified"
            ),
        "closure body edit must remain visible: {value}"
    );
    assert!(
        !value["impact_by_symbol"].as_array().unwrap().is_empty(),
        "body edit must retain impact: {value}"
    );
}

#[test]
fn test_impact_baseline_header_shift_overlapping_positions_has_no_changes() {
    let adjacent = SOURCE.replace(
        "  register(() => { target(); });\n",
        "  register(() => { target(); });\n  register(() => { target(); target(); });\n",
    );
    let tmp = fixture_source(&adjacent);
    // The first closure moves onto the old position of a different body.
    let value = baseline(tmp.path(), &format!("// header\n{adjacent}"));
    assert!(
        value["changed_symbols"].as_array().unwrap().is_empty(),
        "position collisions must still pair by body: {value}"
    );
    assert!(
        value["impact_by_symbol"].as_array().unwrap().is_empty(),
        "header-only movement must not seed impact: {value}"
    );
}

#[test]
fn test_impact_baseline_duplicate_closure_removal_is_reported() {
    let twice = SOURCE.replace(
        "  register(() => { target(); });\n",
        "  register(() => { target(); });\n  register(() => { target(); });\n",
    );
    let tmp = fixture_source(&twice);
    let value = baseline(tmp.path(), &format!("// header\n// second line\n{SOURCE}"));
    let closures: Vec<_> = value["changed_symbols"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| node["name"].as_str().unwrap().starts_with("<anonymous:"))
        .collect();
    assert_eq!(
        closures.len(),
        1,
        "equal bodies must pair one-to-one: {value}"
    );
    assert_eq!(closures[0]["change_type"], "removed");
}

#[test]
fn test_impact_baseline_equal_closure_in_another_file_remains_changed() {
    let tmp = fixture();
    std::fs::write(tmp.path().join("other.ts"), SOURCE).unwrap();
    let value = baseline(tmp.path(), "export function target() {}\n");
    let closures: Vec<_> = value["changed_symbols"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| node["name"].as_str().unwrap().starts_with("<anonymous:"))
        .collect();
    assert_eq!(
        closures.len(),
        2,
        "equal bodies in different files must not pair: {value}"
    );
    assert!(closures
        .iter()
        .any(|node| node["filePath"] == "lib.ts" && node["change_type"] == "removed"));
    assert!(closures
        .iter()
        .any(|node| node["filePath"] == "other.ts" && node["change_type"] == "added"));
}
