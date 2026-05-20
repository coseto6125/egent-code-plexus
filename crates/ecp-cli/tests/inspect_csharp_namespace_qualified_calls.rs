//! Integration test for C# namespace-qualified call resolution.
//!
//! C#'s analyzer emits `NodeKind::Namespace` for every `namespace MyNs { ... }`
//! declaration (capture `namespace.name` in `c_sharp/spec.rs`). Before
//! `is_qualifier` accepted Namespace, calls shaped as
//! `MyNs.SomeStatic.method()` where `MyNs` is the namespace dropped at
//! Tier 2.5 since the qualifier kind didn't pass `is_type`. This pins
//! the cross-language transparent benefit of the resolver fix landed
//! alongside the C++ tests.

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
fn csharp_namespace_qualified_call_resolves() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "App.cs",
        r#"namespace MyApp {
    class Util {
        public static int Compute() { return 7; }
    }

    class Caller {
        public void Run() {
            MyApp.Util.Compute();
        }
    }
}
"#,
    );
    init_and_analyze(repo);

    let result = run_json(repo, &["inspect", "--name", "Compute", "--format", "json"]);
    assert_eq!(result["status"], "found", "{result}");
    let callers = incoming_caller_names(&result);
    assert!(
        callers.iter().any(|n| n == "Run"),
        "namespace-qualified `MyApp.Util.Compute()` must list `Run` as caller; got {callers:?}\nfull={result}"
    );
}
