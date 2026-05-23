//! T1-8: FQN-aware targeting and display in `ecp inspect`.
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

/// T1-8-A: `--name Foo.validate` returns exactly Foo's method, not Bar's.
///
/// Graph: two classes `Foo` and `Bar`, each with a `validate` method.
/// Bare `--name validate` is ambiguous; `--name Foo.validate` must resolve
/// to exactly one result with filePath pointing to foo.py.
#[test]
fn inspect_fqn_targeting_narrows_to_exact_class() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/foo.py",
        "class Foo:\n    def validate(self):\n        return True\n",
    );
    write(
        tmp.path(),
        "src/bar.py",
        "class Bar:\n    def validate(self):\n        return False\n",
    );
    init_and_analyze(tmp.path());

    // Bare name should be ambiguous (both Foo.validate and Bar.validate match).
    let bare = run_json(
        tmp.path(),
        &["inspect", "--name", "validate", "--format", "json"],
    );
    assert_eq!(
        bare["status"], "ambiguous",
        "bare 'validate' must be ambiguous with two classes: {bare}"
    );

    // FQN targeting must narrow to exactly Foo's validate.
    let fqn = run_json(
        tmp.path(),
        &["inspect", "--name", "Foo.validate", "--format", "json"],
    );
    assert_eq!(
        fqn["status"], "found",
        "Foo.validate must resolve to a single found result: {fqn}"
    );
    let file_path = fqn["symbol"]["filePath"].as_str().unwrap_or("");
    assert!(
        file_path.contains("foo.py"),
        "Foo.validate must resolve to foo.py, got filePath={file_path}: {fqn}"
    );
}

/// T1-8-B: `symbol.ownerClass` is present in the JSON output when the symbol
/// has a class owner (method inside a class), absent for top-level functions.
#[test]
fn inspect_output_includes_owner_class_for_methods() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/lib.py",
        "class Greeter:\n    def greet(self):\n        return 'hi'\n\ndef standalone():\n    pass\n",
    );
    init_and_analyze(tmp.path());

    // Method with owner class.
    let method = run_json(
        tmp.path(),
        &["inspect", "--name", "greet", "--format", "json"],
    );
    assert_eq!(method["status"], "found", "{method}");
    let owner = method["symbol"]["ownerClass"].as_str().unwrap_or("");
    assert_eq!(
        owner, "Greeter",
        "greet() must carry ownerClass=Greeter in symbol block, got={owner:?}: {method}"
    );

    // Top-level function: ownerClass must be absent or null.
    let func = run_json(
        tmp.path(),
        &["inspect", "--name", "standalone", "--format", "json"],
    );
    assert_eq!(func["status"], "found", "{func}");
    let owner_standalone = &func["symbol"]["ownerClass"];
    assert!(
        owner_standalone.is_null(),
        "standalone() must have null ownerClass, got={owner_standalone}: {func}"
    );
}

/// T1-8-C: Backward compatibility — bare `--name validate` still resolves
/// when only ONE class has that method name.
#[test]
fn inspect_bare_name_still_works_single_match() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/foo.py",
        "class Foo:\n    def unique_method(self):\n        return 42\n",
    );
    init_and_analyze(tmp.path());

    let result = run_json(
        tmp.path(),
        &["inspect", "--name", "unique_method", "--format", "json"],
    );
    assert_eq!(
        result["status"], "found",
        "bare name for unique method must still work: {result}"
    );
    assert_eq!(
        result["symbol"]["name"], "unique_method",
        "symbol name must match: {result}"
    );
}

/// T1-8-D: Incoming and outgoing edge entries include `ownerClass` when the
/// caller/callee is a class method.
#[test]
fn inspect_edge_entries_include_owner_class() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/lib.py",
        r#"class Foo:
    def run(self):
        return self.helper()

    def helper(self):
        return 1
"#,
    );
    init_and_analyze(tmp.path());

    let result = run_json(
        tmp.path(),
        &["inspect", "--name", "Foo.helper", "--format", "json"],
    );
    assert_eq!(result["status"], "found", "{result}");

    // Foo.run should appear as an incoming caller. Each incoming call entry
    // must carry ownerClass="Foo" since Foo.run is a method of Foo.
    let calls = result["incoming"]["calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !calls.is_empty(),
        "Foo.helper must have Foo.run as incoming caller: {result}"
    );
    for entry in &calls {
        let entry_owner = entry["ownerClass"].as_str().unwrap_or("");
        assert_eq!(
            entry_owner, "Foo",
            "caller entry for Foo.run must carry ownerClass=Foo, got={entry_owner:?}: {entry}"
        );
    }
}
