//! `ecp inspect` must print the same 1-based `startLine`/`endLine` that
//! `ecp cypher` returns: graph spans are 0-based tree-sitter rows, so
//! `endLine` needs the `+1` that `Node::end_line()` applies.

mod common;

use common::{ecp_bin, init_and_analyze, write};

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn inspect_symbol(repo: &Path, name: &str) -> Value {
    let out = Command::new(ecp_bin())
        .args(["inspect", "--name", name, "--format", "json"])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("command failed to spawn");
    assert!(
        out.status.success(),
        "inspect {name} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("inspect {name} did not return JSON\nstdout={stdout}"));
    let v: Value = serde_json::from_str(&stdout[json_start..]).unwrap();
    v["symbol"].clone()
}

fn span(sym: &Value) -> (u64, u64) {
    (
        sym["startLine"].as_u64().unwrap(),
        sym["endLine"].as_u64().unwrap(),
    )
}

const SRC: &str = "class K {\n void audit() {}\n void save(int id) {\n  audit();\n }\n}\n";

#[test]
fn test_inspect_symbol_multiline_method_end_line_is_one_based() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "K.java", SRC);
    init_and_analyze(tmp.path());
    assert_eq!(span(&inspect_symbol(tmp.path(), "save")), (3, 5));
}

#[test]
fn test_inspect_symbol_single_line_method_end_equals_start() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "K.java", SRC);
    init_and_analyze(tmp.path());
    assert_eq!(span(&inspect_symbol(tmp.path(), "audit")), (2, 2));
}

#[test]
fn test_inspect_symbol_ending_on_last_file_line_reports_that_line() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "K.java", SRC);
    init_and_analyze(tmp.path());
    assert_eq!(span(&inspect_symbol(tmp.path(), "K")), (1, 6));
}
