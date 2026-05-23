//! Integration test for PHP namespace-qualified call resolution.
//!
//! PHP's analyzer emits `NodeKind::Namespace` for every `namespace App;`
//! / `namespace App { ... }` declaration (capture `name.namespace` in
//! `php/spec.rs`). Before `is_qualifier` accepted Namespace, calls
//! shaped as `App\helper()` where `App` is the namespace dropped at
//! Tier 2.5 since the qualifier kind didn't pass `is_type`. Pinned
//! here alongside the C++ and C# cases.

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
fn php_namespace_qualified_call_resolves() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "app.php",
        r#"<?php
namespace App {
    function helper() {
        return 7;
    }

    function caller() {
        return \App\helper();
    }
}
"#,
    );
    init_and_analyze(repo);

    let result = run_json(repo, &["inspect", "--name", "helper", "--format", "json"]);
    assert_eq!(result["status"], "found", "{result}");
    let callers = incoming_caller_names(&result);
    assert!(
        callers.iter().any(|n| n == "caller"),
        "namespace-qualified `\\App\\helper()` must list `caller` as caller; got {callers:?}\nfull={result}"
    );
}
