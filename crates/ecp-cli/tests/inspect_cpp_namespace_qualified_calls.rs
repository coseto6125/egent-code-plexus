//! Integration tests for C++ namespace-qualified call resolution.
//!
//! Pre-fix behaviour: `resolve_qualifier_file` Tier 1/2/3 only accepted
//! `ResolveTarget::Type` (Class | Struct | Enum | Typedef | Trait |
//! Interface) as the qualifier kind — `NodeKind::Namespace` was filtered
//! out. Every C++ call shaped as `namespace_name::func()` therefore fell
//! through Tier 2.5, missed the per-file scoping, and either dropped
//! entirely or mis-resolved via Tier 3's bare-name global fallback.
//!
//! The fix adds `ResolveTarget::Qualifier` whose predicate accepts both
//! Type AND Namespace, used by `resolve_qualifier_file`. C++ namespace
//! calls then bind through the file containing the namespace declaration.
//! Inline-namespace calls (`outer::Widget` where `Widget` lives in
//! `inline namespace v1`) work as a side effect: members of an inline
//! namespace share the same source file as their enclosing namespace, so
//! the file-based lookup transparently reaches them — matching the C++
//! standard semantics of inline namespace transparency.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn write(repo: &Path, rel: &str, body: &str) {
    let full = repo.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(full, body).unwrap();
}

fn init_and_analyze(repo: &Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());
    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

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
    let start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("{args:?} did not return JSON\nstdout={stdout}"));
    serde_json::from_str(&stdout[start..])
        .unwrap_or_else(|err| panic!("{args:?} did not return JSON: {err}\nstdout={stdout}"))
}

fn incoming_caller_names(result: &Value) -> Vec<String> {
    result["incoming"]["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["name"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn cpp_regular_namespace_qualified_call_resolves() {
    // Plain (non-inline) namespace: `outer::compute()` must bind to the
    // `compute` declared inside `namespace outer { ... }`. Without the
    // Namespace-as-qualifier fix this call drops on the floor — `outer`
    // is a Namespace node, Tier 2.5 only accepts Type qualifiers, so
    // `resolve_qualifier_file` returns None and the bare-name Tier 3
    // fallback rejects `outer.compute` because that string isn't a
    // registered symbol.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "src/lib.cpp",
        r#"namespace outer {
    int compute() { return 42; }
}

void use_compute() {
    outer::compute();
}
"#,
    );
    init_and_analyze(repo);

    let result = run_json(repo, &["inspect", "--name", "compute", "--format", "json"]);
    assert_eq!(result["status"], "found", "{result}");
    let callers = incoming_caller_names(&result);
    assert!(
        callers.iter().any(|n| n == "use_compute"),
        "expected `use_compute` in incoming.calls of `outer::compute`; got {callers:?}\nfull={result}"
    );
}

#[test]
fn cpp_inline_namespace_skipped_qualifier_resolves() {
    // `inline namespace v1 { class Widget {}; }` inside `namespace outer`
    // means `outer::Widget` is the C++-standard way to name v1::Widget.
    // The resolver doesn't model namespace nesting per-symbol — it works
    // off file containment — so once Namespace is a valid qualifier kind,
    // `outer::Widget` correctly resolves to `Widget` (in the same file
    // as `outer`).
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "src/lib.cpp",
        r#"namespace outer {
    inline namespace v1 {
        class Widget {
        public:
            void draw() {}
        };
        int compute() { return 7; }
    }
}

void use_widget() {
    outer::Widget w;
    w.draw();
    outer::compute();
}
"#,
    );
    init_and_analyze(repo);

    // `outer::compute()` should attach `use_widget` to `compute`'s incoming.
    let compute = run_json(repo, &["inspect", "--name", "compute", "--format", "json"]);
    assert_eq!(compute["status"], "found", "{compute}");
    let compute_callers = incoming_caller_names(&compute);
    assert!(
        compute_callers.iter().any(|n| n == "use_widget"),
        "inline-ns `outer::compute()` must list `use_widget` as caller; got {compute_callers:?}\nfull={compute}"
    );

    // `w.draw()` (member call on a typed local) should attach to `draw`.
    let draw = run_json(repo, &["inspect", "--name", "draw", "--format", "json"]);
    assert_eq!(draw["status"], "found", "{draw}");
    let draw_callers = incoming_caller_names(&draw);
    assert!(
        draw_callers.iter().any(|n| n == "use_widget"),
        "`w.draw()` must list `use_widget`; got {draw_callers:?}\nfull={draw}"
    );
}

#[test]
fn cpp_fully_qualified_inline_namespace_still_resolves() {
    // `outer::v1::compute()` — explicit form — must also resolve. This
    // checks that the new Namespace-as-qualifier rule doesn't break the
    // existing behaviour where a multi-segment qualifier folds to its
    // last segment (`split_qualifier` picks `v1`).
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "src/lib.cpp",
        r#"namespace outer {
    inline namespace v1 {
        int compute() { return 1; }
    }
}

void caller() {
    outer::v1::compute();
}
"#,
    );
    init_and_analyze(repo);

    let result = run_json(repo, &["inspect", "--name", "compute", "--format", "json"]);
    assert_eq!(result["status"], "found", "{result}");
    let callers = incoming_caller_names(&result);
    assert!(
        callers.iter().any(|n| n == "caller"),
        "explicit `outer::v1::compute()` must list `caller`; got {callers:?}\nfull={result}"
    );
}
