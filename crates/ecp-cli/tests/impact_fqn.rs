//! T1-9: FQN-aware targeting and display in `ecp impact`.
//!
//! Test-first: these tests were written before the implementation.
//! Each test verifies a specific gap in the current bare-name-only matching.

use crate::common;

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

/// A Python file with a nested fn `inner` in `outer`, a nested fn `deep` in
/// `inner`, and a nested fn `helper` in the method `K.m`.
const NESTED_PY: &str = r#"def outer():
    def inner():
        def deep():
            return 1
        return deep()
    return inner()


class K:
    def m(self):
        def helper():
            return 2
        return helper()
"#;

/// Upstream impact rows of `target` as JSON.
fn upstream_rows(repo: &Path, target: &str) -> Vec<Value> {
    let result = run_json(
        repo,
        &[
            "impact",
            "--target",
            target,
            "--direction",
            "upstream",
            "--format",
            "json",
        ],
    );
    assert_eq!(result["status"], "success", "{result}");
    result["impact"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("no impact rows: {result}"))
}

/// `(ownerClass, lexicalParent)` of the row named `name` at `depth`.
fn owner_pair(rows: &[Value], name: &str, depth: u64) -> (Value, Value) {
    let row = rows
        .iter()
        .find(|r| r["name"] == name && r["depth"] == depth)
        .unwrap_or_else(|| panic!("no row {name} at depth {depth}: {rows:?}"));
    (row["ownerClass"].clone(), row["lexicalParent"].clone())
}

/// Stands for: an `ecp impact` row prints a nested fn's stored owner as
/// `ownerClass` (null: a scoped node has none) and its enclosing fn chain
/// as `lexicalParent`, at depth 0 and as a caller; a file-level caller
/// prints null for both (FU-2026-10-05-ff801e095d23). Failed before:
/// `ownerClass` held the display chain (bfs.rs `merged.display_owner` in
/// both emission branches) and `lexicalParent` was absent.
#[test]
fn test_impact_json_nested_fn_owner_class_null_lexical_parent_chain() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/lib.py", NESTED_PY);
    init_and_analyze(tmp.path());

    let rows = upstream_rows(tmp.path(), "deep");
    assert_eq!(
        owner_pair(&rows, "deep", 0),
        (Value::Null, Value::from("outer::inner"))
    );
    assert_eq!(
        owner_pair(&rows, "inner", 1),
        (Value::Null, Value::from("outer"))
    );
    assert_eq!(owner_pair(&rows, "outer", 2), (Value::Null, Value::Null));
}

/// Stands for: a nested fn in the method `K.m` prints no `ownerClass` and
/// the `lexicalParent` `K::m`; its caller, the method, prints the
/// `ownerClass` `K` and the display owner `K` as `lexicalParent`.
#[test]
fn test_impact_json_nested_fn_in_method_lexical_parent_class_method_chain() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/lib.py", NESTED_PY);
    init_and_analyze(tmp.path());

    let rows = upstream_rows(tmp.path(), "helper");
    assert_eq!(
        owner_pair(&rows, "helper", 0),
        (Value::Null, Value::from("K::m"))
    );
    assert_eq!(
        owner_pair(&rows, "m", 1),
        (Value::from("K"), Value::from("K"))
    );
}

/// Stands for: an impact row adds `lexicalParent` and renames or removes no
/// field (machine-parsed output): every row carries both owner fields, and
/// the start row carries every other field it carried before.
#[test]
fn test_impact_json_row_adds_lexical_parent_keeps_other_fields() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/lib.py", NESTED_PY);
    init_and_analyze(tmp.path());

    let rows = upstream_rows(tmp.path(), "deep");
    for row in &rows {
        for field in ["ownerClass", "lexicalParent"] {
            assert!(row.get(field).is_some(), "{field} missing: {row}");
        }
    }
    for field in [
        "uid",
        "name",
        "kind",
        "filePath",
        "line",
        "depth",
        "viaReason",
        "viaConfidence",
        "test",
        "requires_verification",
    ] {
        assert!(rows[0].get(field).is_some(), "{field} missing: {}", rows[0]);
    }
}
