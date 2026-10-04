//! Every caller-reading command counts a type's instantiators.
//!
//! A construction (`Widget(1)`) emits its `Calls` edge to the type's sole
//! constructor when it has one, else to the type. `ecp impact` seeds a type
//! with its constructors; `inspect`, `find` (count + hook `callers` list) and
//! `path` must answer the same "who uses Widget" question the same way.
//!
//! Contract the string assertions stand in for: an instantiator reaches the
//! type through its constructor in every reader; a type without a constructor
//! and a non-type node read exactly their own in-edges.

mod common;

use common::{ecp_bin, init_and_analyze, write};

use ecp_cli::commands::find::{compute_hits, FindArgs, FindMode, Hit};
use ecp_cli::engine::Engine;
use ecp_cli::search::TantivyEngine;
use ecp_core::graph::{NodeKind, RelType};
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

const PY_WIDGET: &str = "class Widget:
    def __init__(self, x):
        self.x = x


class Gadget:
    pass


class Lonely:
    def __init__(self):
        self.ready = True
";

const PY_APP: &str = "from .widget import Widget, Gadget


def make_widget():
    return Widget(1)


def make_gadget():
    return Gadget()


def plain():
    return 1


def call_plain():
    return plain()
";

fn python_repo(repo: &Path) {
    write(repo, "pkg/widget.py", PY_WIDGET);
    write(repo, "pkg/app.py", PY_APP);
    init_and_analyze(repo);
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
        .unwrap_or_else(|e| panic!("{args:?} did not return JSON: {e}\nstdout={stdout}"))
}

fn inspect(repo: &Path, name: &str) -> Value {
    run_json(
        repo,
        &["inspect", "--name", name, "--repo", ".", "--format", "json"],
    )
}

fn incoming_calls(result: &Value) -> Vec<Value> {
    result["incoming"]["calls"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn upstream_names(result: &Value) -> Vec<String> {
    result["impact_upstream_1hop"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn test_inspect_class_with_constructor_lists_instantiator_via_constructor() {
    let tmp = tempdir().unwrap();
    python_repo(tmp.path());

    let result = inspect(tmp.path(), "Widget");
    let calls = incoming_calls(&result);
    let entry = calls
        .iter()
        .find(|e| e["name"] == "make_widget")
        .unwrap_or_else(|| panic!("instantiator missing from incoming.calls: {result}"));
    assert_eq!(entry["viaConstructor"], "__init__", "{entry}");
    assert!(
        upstream_names(&result).contains(&"make_widget".to_string()),
        "impact_upstream_1hop must agree with incoming: {result}"
    );
}

#[test]
fn test_inspect_class_without_constructor_has_no_via_constructor_field() {
    let tmp = tempdir().unwrap();
    python_repo(tmp.path());

    let result = inspect(tmp.path(), "Gadget");
    let calls = incoming_calls(&result);
    let entry = calls
        .iter()
        .find(|e| e["name"] == "make_gadget")
        .unwrap_or_else(|| panic!("direct instantiator missing: {result}"));
    assert!(entry.get("viaConstructor").is_none(), "{entry}");
    assert_eq!(upstream_names(&result), vec!["make_gadget".to_string()]);
}

#[test]
fn test_inspect_constructor_without_callers_adds_nothing() {
    let tmp = tempdir().unwrap();
    python_repo(tmp.path());

    let result = inspect(tmp.path(), "Lonely");
    assert!(
        incoming_calls(&result)
            .iter()
            .all(|e| e.get("viaConstructor").is_none()),
        "no instantiator, so no constructor-sourced entry: {result}"
    );
    assert!(upstream_names(&result).is_empty(), "{result}");
}

#[test]
fn test_inspect_function_keeps_exactly_its_own_callers() {
    let tmp = tempdir().unwrap();
    python_repo(tmp.path());

    let result = inspect(tmp.path(), "plain");
    let calls = incoming_calls(&result);
    assert_eq!(calls.len(), 1, "{result}");
    assert_eq!(calls[0]["name"], "call_plain");
    assert!(calls[0].get("viaConstructor").is_none());
    assert_eq!(upstream_names(&result), vec!["call_plain".to_string()]);
}

/// Contract: a class's constructor is read only for its `Calls` in-edges. The
/// owning class's `HasMethod` edge into `__init__` is not an instantiation, so
/// it must neither list the class under `has_method` nor make a sibling class
/// in the same file look like an instantiator.
#[test]
fn test_inspect_class_constructor_non_call_in_edges_not_read() {
    let tmp = tempdir().unwrap();
    write(
        tmp.path(),
        "m.py",
        "class A:
    def __init__(self):
        self.a = 1


class B:
    def __init__(self):
        self.b = 1


def make():
    return A()
",
    );
    init_and_analyze(tmp.path());

    let result = inspect(tmp.path(), "A");
    let incoming = &result["incoming"];
    assert!(
        incoming
            .get("has_method")
            .and_then(Value::as_array)
            .is_none_or(|v| v.iter().all(|e| e.get("viaConstructor").is_none())),
        "no has_method entry may come through the constructor: {result}"
    );
    let calls = incoming_calls(&result);
    assert!(
        calls
            .iter()
            .any(|e| e["name"] == "make" && e["viaConstructor"] == "__init__"),
        "{result}"
    );
    assert_eq!(
        upstream_names(&result),
        vec!["make".to_string()],
        "{result}"
    );
}

fn path(repo: &Path, from: &str, to: &str) -> Value {
    run_json(repo, &["path", from, to, "--repo", ".", "--format", "json"])
}

#[test]
fn test_path_to_class_with_constructor_ends_at_the_constructor() {
    let tmp = tempdir().unwrap();
    python_repo(tmp.path());

    let out = path(tmp.path(), "make_widget", "Widget");
    assert_eq!(out["found"], true, "{out}");
    let steps = out["path"].as_array().unwrap();
    assert_eq!(steps.first().unwrap()["name"], "make_widget", "{out}");
    assert_eq!(
        steps.last().unwrap()["name"],
        "__init__",
        "the route ends at the node it reached: {out}"
    );
    assert_eq!(
        out["toCandidates"], 1,
        "candidate count stays additive-stable: {out}"
    );
}

#[test]
fn test_path_to_class_without_constructor_is_unchanged() {
    let tmp = tempdir().unwrap();
    python_repo(tmp.path());

    let out = path(tmp.path(), "make_gadget", "Gadget");
    assert_eq!(out["found"], true, "{out}");
    assert_eq!(
        out["path"].as_array().unwrap().last().unwrap()["name"],
        "Gadget"
    );
}

#[test]
fn test_path_to_function_is_unchanged() {
    let tmp = tempdir().unwrap();
    python_repo(tmp.path());

    let out = path(tmp.path(), "call_plain", "plain");
    assert_eq!(out["found"], true, "{out}");
    assert_eq!(out["hops"], 1, "{out}");
}

#[test]
fn test_path_from_class_to_unrelated_symbol_still_misses() {
    let tmp = tempdir().unwrap();
    python_repo(tmp.path());

    let out = path(tmp.path(), "plain", "Widget");
    assert_eq!(out["found"], false, "{out}");
}

/// `Widget` (one constructor), `Gadget` (none), `Orphan` (a constructor
/// nobody calls) and a plain function, in one file. `both` calls `Widget`
/// and `Widget.__init__`; `via_ctor` calls only the constructor; `direct`
/// calls only the class.
fn fixture_hits(pattern: &str) -> Vec<Hit> {
    let mut fx = GraphFixture::new();
    let p = "pkg/widget.py";
    let widget = fx.node(NodeKind::Class, p, "Widget");
    let init = fx.node_owned(NodeKind::Constructor, p, "Widget", "__init__");
    let gadget = fx.node(NodeKind::Class, p, "Gadget");
    let orphan = fx.node(NodeKind::Class, p, "Orphan");
    let orphan_init = fx.node_owned(NodeKind::Constructor, p, "Orphan", "__init__");
    let plain = fx.func(p, "plain");
    let both = fx.func(p, "both");
    let via_ctor = fx.func(p, "via_ctor");
    let direct = fx.func(p, "direct");
    fx.edge(widget, init, RelType::HasMethod);
    fx.edge(orphan, orphan_init, RelType::HasMethod);
    fx.edge(both, widget, RelType::Calls);
    fx.edge(both, init, RelType::Calls);
    fx.edge(via_ctor, init, RelType::Calls);
    fx.edge(direct, gadget, RelType::Calls);
    fx.edge(direct, plain, RelType::Calls);
    let graph = fx.build();

    let dir = tempdir().unwrap();
    let bytes = rkyv::to_bytes::<Error>(&graph).expect("rkyv serialize");
    std::fs::write(dir.path().join("graph.bin"), bytes.as_slice()).unwrap();
    TantivyEngine::build_index(dir.path(), &graph).expect("tantivy build");
    let engine = Engine::load(dir.path().join("graph.bin")).expect("engine load");

    let args = FindArgs {
        pattern: Some(pattern.to_string()),
        mode: FindMode::Bm25,
        fuzzy: false,
        all: false,
        include_tests: false,
        kind: None,
        file: None,
        repo: None,
        format: None,
        batch: false,
    };
    compute_hits(args, &engine).expect("compute_hits")
}

fn hit<'a>(hits: &'a [Hit], name: &str) -> &'a Hit {
    hits.iter()
        .find(|h| h.name == name)
        .unwrap_or_else(|| panic!("no hit named {name} in {hits:?}"))
}

#[test]
fn test_find_class_caller_count_includes_constructor_callers_deduplicated() {
    let hits = fixture_hits("Widget");
    let h = hit(&hits, "Widget");
    assert_eq!(h.caller_count, 2, "both counts once, via_ctor once: {h:?}");
    let mut callers = h.callers.clone();
    callers.sort();
    assert_eq!(callers, vec!["both".to_string(), "via_ctor".to_string()]);
}

#[test]
fn test_find_class_without_constructor_counts_its_own_callers() {
    let hits = fixture_hits("Gadget");
    let h = hit(&hits, "Gadget");
    assert_eq!(h.caller_count, 1, "{h:?}");
    assert_eq!(h.callers, vec!["direct".to_string()]);
}

#[test]
fn test_find_class_with_uncalled_constructor_has_no_callers() {
    let hits = fixture_hits("Orphan");
    let h = hit(&hits, "Orphan");
    assert_eq!(h.caller_count, 0, "{h:?}");
    assert!(h.callers.is_empty(), "{h:?}");
}

#[test]
fn test_find_function_keeps_exactly_its_own_callers() {
    let hits = fixture_hits("plain");
    let h = hit(&hits, "plain");
    assert_eq!(h.caller_count, 1, "{h:?}");
    assert_eq!(h.callers, vec!["direct".to_string()]);
}

#[test]
fn test_find_json_caller_count_includes_instantiator_end_to_end() {
    let tmp = tempdir().unwrap();
    python_repo(tmp.path());

    let result = run_json(
        tmp.path(),
        &[
            "find", "Widget", "--mode", "bm25", "--format", "json", "--repo", ".",
        ],
    );
    let h = result["source"]
        .as_array()
        .expect("source bucket")
        .iter()
        .find(|h| h["name"] == "Widget")
        .unwrap_or_else(|| panic!("no Widget hit: {result}"));
    assert_eq!(h["caller_count"], 1, "{h}");
}

/// A fuzzy match over more classes than `find` probes one by one switches to
/// the one-pass constructor index; every class must keep the caller count the
/// per-type probe gives (one instantiator through its `__init__`).
#[test]
fn test_find_fuzzy_many_classes_constructor_index_keeps_caller_counts() {
    let tmp = tempdir().unwrap();
    let mut models = String::new();
    let mut app = String::from("from pkg.models import *\n\n");
    for i in 0..20 {
        models.push_str(&format!(
            "class Gizmo{i}:\n    def __init__(self):\n        self.v = {i}\n\n"
        ));
        app.push_str(&format!("def make{i}():\n    return Gizmo{i}()\n\n"));
    }
    write(tmp.path(), "pkg/__init__.py", "");
    write(tmp.path(), "pkg/models.py", &models);
    write(tmp.path(), "pkg/app.py", &app);
    init_and_analyze(tmp.path());

    let result = run_json(
        tmp.path(),
        &[
            "find", "Gizmo", "--mode", "fuzzy", "--all", "--kind", "class", "--format", "json",
            "--repo", ".",
        ],
    );
    let rows: Vec<&Value> = result
        .as_object()
        .expect("json object")
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .filter(|h| h["name"].as_str().is_some_and(|n| n.starts_with("Gizmo")))
        .collect();
    assert_eq!(rows.len(), 20, "{result}");
    for h in rows {
        assert_eq!(h["caller_count"], 1, "{h}");
    }
}
