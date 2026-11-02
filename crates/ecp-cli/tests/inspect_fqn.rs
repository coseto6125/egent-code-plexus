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

/// A Python file with a nested fn `inner` in `outer` and a nested fn
/// `helper` in the method `K.m`.
const NESTED_PY: &str = r#"def outer():
    def inner():
        return 1
    return inner()


class K:
    def m(self):
        def helper():
            return 2
        return helper()
"#;

/// `(ownerClass, lexicalParent)` of one inspect symbol header or edge entry.
fn owner_pair(entry: &Value) -> (Value, Value) {
    (entry["ownerClass"].clone(), entry["lexicalParent"].clone())
}

/// The `incoming` / `outgoing` `calls` entry named `name`.
fn call_entry<'v>(result: &'v Value, section: &str, name: &str) -> &'v Value {
    result[section]["calls"]
        .as_array()
        .and_then(|calls| calls.iter().find(|e| e["name"] == name))
        .unwrap_or_else(|| panic!("no {section} call entry {name}: {result}"))
}

/// Stands for: `ecp inspect` prints a nested fn's stored owner as
/// `ownerClass` (null: a scoped node has none) and its enclosing fn as
/// `lexicalParent`, in the symbol header and in the edge entries that reach
/// it (FU-2026-10-05-ff801e095d23). Failed before: `ownerClass` held the
/// display chain `outer` (inspect.rs `merged.display_owner` at the three
/// `ownerClass` sites) and `lexicalParent` was absent.
#[test]
fn test_inspect_json_nested_fn_owner_class_null_lexical_parent_outer() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/lib.py", NESTED_PY);
    init_and_analyze(tmp.path());
    let nested = (Value::Null, Value::from("outer"));
    let top = (Value::Null, Value::Null);

    let inner = run_json(tmp.path(), &["inspect", "inner", "--format", "json"]);
    assert_eq!(inner["status"], "found", "{inner}");
    assert_eq!(owner_pair(&inner["symbol"]), nested, "{inner}");
    assert_eq!(
        owner_pair(call_entry(&inner, "incoming", "outer")),
        top,
        "{inner}"
    );

    let outer = run_json(tmp.path(), &["inspect", "outer", "--format", "json"]);
    assert_eq!(outer["status"], "found", "{outer}");
    assert_eq!(owner_pair(&outer["symbol"]), top, "{outer}");
    assert_eq!(
        owner_pair(call_entry(&outer, "outgoing", "inner")),
        nested,
        "{outer}"
    );
}

/// Stands for: a nested fn in the method `K.m` has no `ownerClass` and the
/// `lexicalParent` `K::m`; the method keeps the `ownerClass` `K`, and its
/// `lexicalParent` is the display owner, the stored owner `K`.
#[test]
fn test_inspect_json_nested_fn_in_method_lexical_parent_is_class_method_chain() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/lib.py", NESTED_PY);
    init_and_analyze(tmp.path());

    let helper = run_json(tmp.path(), &["inspect", "helper", "--format", "json"]);
    assert_eq!(helper["status"], "found", "{helper}");
    assert_eq!(
        owner_pair(&helper["symbol"]),
        (Value::Null, Value::from("K::m")),
        "{helper}"
    );
    let method = (Value::from("K"), Value::from("K"));
    assert_eq!(
        owner_pair(call_entry(&helper, "incoming", "m")),
        method,
        "{helper}"
    );

    let m = run_json(tmp.path(), &["inspect", "K.m", "--format", "json"]);
    assert_eq!(m["status"], "found", "{m}");
    assert_eq!(owner_pair(&m["symbol"]), method, "{m}");
}

/// Stands for: the json symbol header adds `lexicalParent` and renames or
/// removes no field (machine-parsed output). The keys print sorted (the
/// json map is ordered by key), so the test compares the set.
#[test]
fn test_inspect_json_symbol_header_adds_lexical_parent_keeps_other_fields() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/lib.py", NESTED_PY);
    init_and_analyze(tmp.path());

    let inner = run_json(tmp.path(), &["inspect", "inner", "--format", "json"]);
    let mut keys: Vec<&str> = inner["symbol"]
        .as_object()
        .unwrap_or_else(|| panic!("no symbol header: {inner}"))
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "decorators",
            "endLine",
            "filePath",
            "kind",
            "lexicalParent",
            "name",
            "ownerClass",
            "startLine",
        ],
        "{inner}"
    );
}
