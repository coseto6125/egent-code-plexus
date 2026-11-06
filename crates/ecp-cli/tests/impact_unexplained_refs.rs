//! `unexplained_refs` across the 14 mainstream languages: per language, one
//! function with one resolved caller (a `Calls` edge) and one reference the
//! graph misses, the function passed as a value. One caller is under
//! `UNEXPLAINED_SCAN_MAX_CALLERS`, so the scan runs without a flag. The
//! answer must list the missed reference, as a `reference` inside `holder`,
//! and only it: the resolved call and the declaration are explained.
//!
//! Every use file has the same four-line shape, so one span layout fits all:
//! line 1 holds `caller` and its resolved call, lines 2-4 hold `holder`, and
//! line 3 holds the missed reference.

use crate::common;

use common::{run_git, write};
use ecp_cli::commands::impact::{
    build_payload, Direction, ImpactArgs, DEFAULT_CONFIDENCE_THRESHOLD,
};
use ecp_cli::engine::Engine;
use ecp_core::graph::RelType;
use ecp_core::graph_fixture::GraphFixture;
use serde_json::{json, Value};

struct Case {
    ext: &'static str,
    name: &'static str,
    /// One line that declares `name`.
    def: &'static str,
    /// Four lines; see the module doc.
    uses: &'static str,
}

fn args(name: &str, file: &str, repo: &str) -> ImpactArgs {
    ImpactArgs {
        name: Some(name.to_string()),
        target: None,
        baseline: None,
        file: Some(file.to_string()),
        kind: None,
        direction: Direction::Up,
        depth: 5,
        high_trust_only: false,
        min_confidence: None,
        include_tests: false,
        exclude_tests: false,
        relation_types: None,
        repo: Some(repo.to_string()),
        test_coverage: false,
        no_heuristic: false,
        confidence_threshold: DEFAULT_CONFIDENCE_THRESHOLD,
        explain_confidence: false,
        format: None,
        literal: None,
        literal_coherence: false,
        batch: false,
        ambiguous_callers: false,
        max_results: None,
    }
}

fn assert_missed_reference_listed(case: Case) {
    let repo = tempfile::tempdir().unwrap();
    let lib = format!("src/lib.{}", case.ext);
    let uses = format!("src/use.{}", case.ext);
    write(repo.path(), &lib, &format!("{}\n", case.def));
    write(repo.path(), &uses, case.uses);
    run_git(repo.path(), &["init", "-q"]);
    run_git(repo.path(), &["add", "-A"]);

    let mut fx = GraphFixture::new();
    let def = fx.func(&lib, case.name);
    fx.span(def, (0, 0, 0, 40));
    let caller = fx.func(&uses, "caller");
    fx.span(caller, (0, 0, 0, 60));
    fx.edge_with(caller, def, RelType::Calls, 1.0, "call");
    let holder = fx.func(&uses, "holder");
    fx.span(holder, (1, 0, 3, 1));

    let graph_dir = tempfile::tempdir().unwrap();
    let graph_path = graph_dir.path().join("graph.bin");
    std::fs::write(&graph_path, fx.into_bytes()).unwrap();
    let engine = Engine::load(&graph_path).expect("engine load");
    let payload = build_payload(
        &args(case.name, &lib, &repo.path().to_string_lossy()),
        &engine,
    )
    .unwrap_or_else(|e| panic!(".{}: impact failed: {e}", case.ext));

    let field = &payload["unexplained_refs"];
    let sites: Vec<Value> = field["sites"]
        .as_array()
        .unwrap_or_else(|| panic!(".{}: scan must run at 1 caller: {payload}", case.ext))
        .clone();
    assert_eq!(
        sites,
        vec![json!({"path": uses, "line": 3, "form": "reference", "enclosing": "holder"})],
        ".{}: only the value reference is unexplained: {payload}",
        case.ext
    );
    assert_eq!(field["total"], 1, ".{}: {payload}", case.ext);
    let callers: Vec<&str> = payload["impact"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["depth"] == 1)
        .filter_map(|row| row["name"].as_str())
        .collect();
    assert_eq!(callers, ["caller"], ".{}: {payload}", case.ext);
}

#[test]
fn test_unexplained_refs_typescript_value_reference_listed() {
    assert_missed_reference_listed(Case {
        ext: "ts",
        name: "fetchItem",
        def: "export function fetchItem(n: number) { return n; }",
        uses: "export function caller() { return fetchItem(1); }\n\
               export function holder() {\n  return [fetchItem];\n}\n",
    });
}

#[test]
fn test_unexplained_refs_javascript_value_reference_listed() {
    assert_missed_reference_listed(Case {
        ext: "js",
        name: "fetchItem",
        def: "export function fetchItem(n) { return n; }",
        uses: "export function caller() { return fetchItem(1); }\n\
               export function holder() {\n  return { handler: fetchItem };\n}\n",
    });
}

#[test]
fn test_unexplained_refs_python_value_reference_listed() {
    assert_missed_reference_listed(Case {
        ext: "py",
        name: "fetch_item",
        def: "def fetch_item(n): return n",
        uses: "def caller(): return fetch_item(1)\n\
               def holder():\n    return [fetch_item]\n# end\n",
    });
}

#[test]
fn test_unexplained_refs_java_method_reference_listed() {
    assert_missed_reference_listed(Case {
        ext: "java",
        name: "fetchItem",
        def: "class Lib { static Item fetchItem(int n) { return null; } }",
        uses: "class Use { Item caller() { return Lib.fetchItem(1); }\n\
               Function<Integer, Item> holder() {\n    return Lib::fetchItem;\n} }\n",
    });
}

#[test]
fn test_unexplained_refs_kotlin_callable_reference_listed() {
    assert_missed_reference_listed(Case {
        ext: "kt",
        name: "fetchItem",
        def: "fun fetchItem(n: Int): Item = Item(n)",
        uses: "fun caller() = fetchItem(1)\n\
               fun holder(): (Int) -> Item {\n    return ::fetchItem\n}\n",
    });
}

#[test]
fn test_unexplained_refs_csharp_method_group_listed() {
    assert_missed_reference_listed(Case {
        ext: "cs",
        name: "FetchItem",
        def: "static class Lib { public static Item FetchItem(int n) => null; }",
        uses: "class Use { Item Caller() => Lib.FetchItem(1);\n\
               Func<int, Item> Holder() {\n    return Lib.FetchItem;\n} }\n",
    });
}

#[test]
fn test_unexplained_refs_go_function_value_listed() {
    assert_missed_reference_listed(Case {
        ext: "go",
        name: "FetchItem",
        def: "func FetchItem(n int) Item { return Item{} }",
        uses: "func caller() Item { return FetchItem(1) }\n\
               func holder() func(int) Item {\n\treturn FetchItem\n}\n",
    });
}

#[test]
fn test_unexplained_refs_rust_fn_pointer_listed() {
    assert_missed_reference_listed(Case {
        ext: "rs",
        name: "fetch_item",
        def: "pub fn fetch_item(n: u32) -> Item { Item(n) }",
        uses: "fn caller() -> Item { fetch_item(1) }\n\
               fn holder() -> fn(u32) -> Item {\n    fetch_item\n}\n",
    });
}

#[test]
fn test_unexplained_refs_php_string_callable_listed() {
    assert_missed_reference_listed(Case {
        ext: "php",
        name: "fetch_item",
        def: "<?php function fetch_item($n) { return $n; }",
        uses: "<?php function caller() { return fetch_item(1); }\n\
               function holder() {\n    return array_map('fetch_item', [1]);\n}\n",
    });
}

#[test]
fn test_unexplained_refs_ruby_method_symbol_listed() {
    assert_missed_reference_listed(Case {
        ext: "rb",
        name: "fetch_item",
        def: "def fetch_item(n) = n",
        uses: "def caller; fetch_item(1); end\n\
               def holder\n  [1].map(&method(:fetch_item))\nend\n",
    });
}

#[test]
fn test_unexplained_refs_swift_function_value_listed() {
    assert_missed_reference_listed(Case {
        ext: "swift",
        name: "fetchItem",
        def: "func fetchItem(_ n: Int) -> Item { Item(n) }",
        uses: "func caller() -> Item { return fetchItem(1) }\n\
               func holder() -> (Int) -> Item {\n    return fetchItem\n}\n",
    });
}

#[test]
fn test_unexplained_refs_c_function_pointer_listed() {
    assert_missed_reference_listed(Case {
        ext: "c",
        name: "fetch_item",
        def: "Item fetch_item(int n) { Item i = {n}; return i; }",
        uses: "Item caller(void) { return fetch_item(1); }\n\
               item_fn holder(void) {\n    return fetch_item;\n}\n",
    });
}

#[test]
fn test_unexplained_refs_cpp_address_of_function_listed() {
    assert_missed_reference_listed(Case {
        ext: "cpp",
        name: "fetch_item",
        def: "Item fetch_item(int n) { return Item{n}; }",
        uses: "Item caller() { return fetch_item(1); }\n\
               ItemFn holder() {\n    return &fetch_item;\n}\n",
    });
}

#[test]
fn test_unexplained_refs_dart_tear_off_listed() {
    assert_missed_reference_listed(Case {
        ext: "dart",
        name: "fetchItem",
        def: "Item fetchItem(int n) => Item(n);",
        uses: "Item caller() => fetchItem(1);\n\
               Item Function(int) holder() {\n  return fetchItem;\n}\n",
    });
}
