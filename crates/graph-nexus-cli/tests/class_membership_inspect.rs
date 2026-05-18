//! E2E fixture tests for class-membership post-process: verifies that
//! `gnx inspect <Class>` surfaces `contained_methods` / `contained_properties`
//! across 5 representative language idioms (TS / Ruby / Python / Rust trait
//! impl / Rust inherent impl) — plus a cypher behaviour test pinning the B.1
//! emission convention (`HasMethod` target kind is parser-determined, not
//! filtered in the query).

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
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

    let out = Command::new(gnx_bin())
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
    let out = Command::new(gnx_bin())
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

fn member_names(arr: &Value) -> Vec<String> {
    arr.as_array()
        .map(|xs| {
            xs.iter()
                .filter_map(|e| e.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ── TypeScript class ──────────────────────────────────────────────────────

#[test]
fn typescript_class_emits_methods() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/foo.ts",
        "export class Foo {\n  x: number = 1;\n  y: string = 'hi';\n  bar() { return this.x; }\n  baz() { return this.y; }\n}\n",
    );
    init_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &["inspect", "--name", "Foo", "--format", "json"],
    );
    assert_eq!(out["status"].as_str(), Some("found"));

    let methods = member_names(&out["contained_methods"]);
    assert!(
        methods.iter().any(|n| n == "bar"),
        "expected 'bar' in contained_methods, got {methods:?}"
    );
    assert!(
        methods.iter().any(|n| n == "baz"),
        "expected 'baz' in contained_methods, got {methods:?}"
    );
    // NOTE: TypeScript parser currently doesn't emit NodeKind::Property
    // for class fields — Property emission across languages varies (Go/C++/
    // Swift/Ruby/Dart/C cover it). Property coverage is asserted via the
    // Go test below; TS gap is pre-existing and out-of-scope for this PR.
}

// ── Go struct fields → Property ───────────────────────────────────────────

#[test]
fn go_struct_emits_properties() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "go.mod", "module sample\n\ngo 1.21\n");
    write(
        tmp.path(),
        "main.go",
        "package main\n\n\
         type Foo struct {\n\
             X int\n\
             Y string\n\
         }\n",
    );
    init_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &["inspect", "--name", "Foo", "--format", "json"],
    );
    assert_eq!(out["status"].as_str(), Some("found"));

    let props = member_names(&out["contained_properties"]);
    assert!(
        props.iter().any(|n| n == "X"),
        "Go struct field 'X' must surface as contained_property, got {props:?}"
    );
}

// ── Ruby class ────────────────────────────────────────────────────────────

#[test]
fn ruby_class_emits_methods() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "lib/foo.rb",
        "class Foo\n  def bar\n    42\n  end\n  def baz\n    'hi'\n  end\nend\n",
    );
    init_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &["inspect", "--name", "Foo", "--format", "json"],
    );
    assert_eq!(out["status"].as_str(), Some("found"));

    let methods = member_names(&out["contained_methods"]);
    assert!(
        methods.iter().any(|n| n == "bar"),
        "expected 'bar' in contained_methods, got {methods:?}"
    );
    assert!(
        methods.iter().any(|n| n == "baz"),
        "expected 'baz' in contained_methods, got {methods:?}"
    );
}

// ── Python class — B.1: `def` inside class emits as Method (parity-14-langs) ─

#[test]
fn python_class_def_emits_as_method_kind() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/foo.py",
        "class Foo:\n    x = 1\n    def bar(self):\n        return self.x\n",
    );
    init_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &["inspect", "--name", "Foo", "--format", "json"],
    );
    assert_eq!(out["status"].as_str(), Some("found"));

    let methods = out["contained_methods"].as_array().unwrap();
    let bar = methods
        .iter()
        .find(|m| m["name"].as_str() == Some("bar"))
        .unwrap_or_else(|| panic!("expected 'bar' in contained_methods, got {methods:?}"));
    assert_eq!(
        bar["kind"].as_str(),
        Some("Method"),
        "Python class-internal `def` must classify as Method (parity-14-langs fix)",
    );
}

// ── Rust trait impl: `impl Display for Foo { fn fmt(&self,_) {} }` ────────

#[test]
fn rust_trait_impl_bridges_to_struct() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "Cargo.toml",
        "[package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        tmp.path(),
        "src/lib.rs",
        "use std::fmt;\n\
         pub struct Foo;\n\
         impl fmt::Display for Foo {\n\
             fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {\n\
                 write!(f, \"foo\")\n\
             }\n\
         }\n",
    );
    init_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &["inspect", "--name", "Foo", "--format", "json"],
    );
    assert_eq!(
        out["status"].as_str(),
        Some("found"),
        "Foo struct must resolve: {out}"
    );

    let methods = member_names(&out["contained_methods"]);
    assert!(
        methods.iter().any(|n| n == "fmt"),
        "trait impl 'fmt' must surface on Foo via Pass 2 bridge, got {methods:?}"
    );
}

// ── Rust inherent impl: `impl Foo { fn new() -> Self {} }` ────────────────

#[test]
fn rust_inherent_impl_bridges_to_struct() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "Cargo.toml",
        "[package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        tmp.path(),
        "src/lib.rs",
        "pub struct Foo;\n\
         impl Foo {\n\
             pub fn new() -> Self {\n\
                 Foo\n\
             }\n\
             pub fn answer(&self) -> u32 {\n\
                 42\n\
             }\n\
         }\n",
    );
    init_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &["inspect", "--name", "Foo", "--format", "json"],
    );
    assert_eq!(out["status"].as_str(), Some("found"));

    let methods = member_names(&out["contained_methods"]);
    assert!(
        methods.iter().any(|n| n == "new"),
        "inherent fn 'new' must surface on Foo, got {methods:?}"
    );
    assert!(
        methods.iter().any(|n| n == "answer"),
        "inherent fn 'answer' must surface on Foo, got {methods:?}"
    );
}

// ── Cypher B.1 convention: target kind not filtered ───────────────────────

#[test]
fn cypher_has_method_b1_kind_agnostic() {
    let tmp = tempfile::tempdir().unwrap();
    // Mix Python (def → Function) + TS (method → Method) to prove a single
    // query catches both.
    write(
        tmp.path(),
        "src/a.py",
        "class PyFoo:\n    def py_bar(self):\n        return 1\n",
    );
    write(
        tmp.path(),
        "src/b.ts",
        "export class TsFoo {\n  ts_bar() { return 1; }\n}\n",
    );
    init_and_analyze(tmp.path());

    let out_all = run_json(
        tmp.path(),
        &[
            "cypher",
            "MATCH (a:Class)-[:HasMethod]->(b) RETURN a.name, b.name, b.kind",
            "--format",
            "json",
        ],
    );
    let rows_all = out_all["rows"].as_array().unwrap();
    let names_all: Vec<&str> = rows_all.iter().filter_map(|r| r[1].as_str()).collect();
    assert!(
        names_all.contains(&"py_bar"),
        "kind-agnostic query must catch Python def, got {names_all:?}"
    );
    assert!(
        names_all.contains(&"ts_bar"),
        "kind-agnostic query must catch TS method, got {names_all:?}"
    );

    // Reverse: querying with `:Method` filter intentionally misses Python def.
    let out_strict = run_json(
        tmp.path(),
        &[
            "cypher",
            "MATCH (a:Class)-[:HasMethod]->(b:Method) RETURN b.name",
            "--format",
            "json",
        ],
    );
    let rows_strict = out_strict["rows"].as_array().unwrap();
    let names_strict: Vec<&str> = rows_strict.iter().filter_map(|r| r[0].as_str()).collect();
    assert!(
        !names_strict.contains(&"py_bar"),
        "strict `:Method` filter must skip Python def (B.1 documented behaviour), got {names_strict:?}"
    );
}
