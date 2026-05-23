//! T1-9: FQN-aware targeting and display in `ecp impact`.
//!
//! Test-first: these tests were written before the implementation.
//! Each test verifies a specific gap in the current bare-name-only matching.

mod common;

use common::{ecp_bin, init_and_analyze, write};

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn run_json(repo: &Path, args: &[&str]) -> Value {
    let out = Command::new(ecp_bin())
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("command failed to spawn");
    assert!(
        out.status.success(),
        "{args:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("{args:?} did not return JSON\nstdout={stdout}"));
    serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|err| panic!("{args:?} did not return JSON: {err}\nstdout={stdout}"))
}

/// T1-9-A: `--target Foo.validate` scopes BFS to Foo's method only.
///
/// Graph: Foo.validate called by Foo.run; Bar.validate called by Bar.run.
/// `--target Foo.validate --direction up` must include Foo.run in impact
/// and NOT include Bar.run.
#[test]
fn impact_fqn_targeting_scopes_bfs_to_correct_method() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/foo.py",
        r#"class Foo:
    def validate(self):
        return True

    def run(self):
        return self.validate()
"#,
    );
    write(
        tmp.path(),
        "src/bar.py",
        r#"class Bar:
    def validate(self):
        return False

    def run(self):
        return self.validate()
"#,
    );
    init_and_analyze(tmp.path());

    // FQN-targeted impact must only surface Foo.run as upstream caller.
    let result = run_json(
        tmp.path(),
        &[
            "impact",
            "--target",
            "Foo.validate",
            "--direction",
            "up",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        result["status"], "success",
        "Foo.validate impact must succeed: {result}"
    );

    let impact = result["impact"].as_array().unwrap();
    let names: Vec<&str> = impact.iter().filter_map(|e| e["name"].as_str()).collect();

    assert!(
        names.iter().any(|&n| n == "run" || n == "Foo.run"),
        "impact must include Foo.run as caller of Foo.validate, got names={names:?}: {result}"
    );

    // Bar.run must NOT appear — it only calls Bar.validate, not Foo.validate.
    let files: Vec<&str> = impact
        .iter()
        .filter_map(|e| e["filePath"].as_str())
        .collect();
    let bar_run_present = impact.iter().any(|e| {
        e["name"].as_str() == Some("run") && e["filePath"].as_str().unwrap_or("").contains("bar.py")
    });
    assert!(
        !bar_run_present,
        "Bar.run must NOT appear in Foo.validate impact: files={files:?}: {result}"
    );
}

/// T1-9-B: BFS impact entries carry `ownerClass` so LLM consumers can
/// distinguish `Foo.run` from `Bar.run` without checking filePath.
#[test]
fn impact_entries_carry_owner_class() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/lib.py",
        r#"class Service:
    def process(self):
        return self.validate()

    def validate(self):
        return True
"#,
    );
    init_and_analyze(tmp.path());

    let result = run_json(
        tmp.path(),
        &[
            "impact",
            "--target",
            "Service.validate",
            "--direction",
            "up",
            "--format",
            "json",
        ],
    );
    assert_eq!(result["status"], "success", "{result}");

    let impact = result["impact"].as_array().unwrap();
    // Find the `process` caller entry (depth > 0).
    let process_entry = impact
        .iter()
        .find(|e| e["name"].as_str() == Some("process") && e["depth"].as_u64().unwrap_or(0) > 0);
    let process_entry = process_entry
        .unwrap_or_else(|| panic!("process must appear in upstream impact of validate: {result}"));

    let owner = process_entry["ownerClass"].as_str().unwrap_or("");
    assert_eq!(
        owner, "Service",
        "process entry must carry ownerClass=Service, got={owner:?}: {process_entry}"
    );
}

/// T1-9-C: Backward compatibility — bare `--target validate` still resolves
/// when only one symbol has that name.
#[test]
fn impact_bare_target_still_works_single_match() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/lib.py",
        r#"def only_validate():
    return True

def caller():
    return only_validate()
"#,
    );
    init_and_analyze(tmp.path());

    let result = run_json(
        tmp.path(),
        &[
            "impact",
            "--target",
            "only_validate",
            "--direction",
            "up",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        result["status"], "success",
        "bare target must still work for unique name: {result}"
    );
    let impact = result["impact"].as_array().unwrap();
    assert!(
        impact.iter().any(|e| e["name"].as_str() == Some("caller")),
        "caller must appear in upstream impact of only_validate: {result}"
    );
}
