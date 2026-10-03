use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use std::path::Path;

const IMPACT_MOD: &str = r#"
pub struct ImpactArgs { pub flag: bool }
impl ImpactArgs {
    pub fn walks_tests(&self) -> bool { self.flag }
}
"#;

const WALK_MOD: &str = r#"
pub struct ImpactArgs { pub depth: u32 }
impl ImpactArgs {
    pub fn walks_tests(&self) -> bool { self.depth > 0 }
}
"#;

fn build(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let provider = RustProvider::new().expect("RustProvider::new");
    let mut builder = GraphBuilder::new();
    for (path, src) in files {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), src.as_bytes())
                .expect("parse_file"),
        );
    }
    builder.build()
}

/// `build` plus one TypeScript file.
fn build_with_ts(rust: &[(String, &str)], ts_path: &str, ts_src: &str) -> ZeroCopyGraph {
    let provider = RustProvider::new().expect("RustProvider::new");
    let mut builder = GraphBuilder::new();
    for (path, src) in rust {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), src.as_bytes())
                .expect("parse_file"),
        );
    }
    let ts = TypeScriptProvider::new().expect("TypeScriptProvider::new");
    builder.add_graph(
        ts.parse_file(Path::new(ts_path), ts_src.as_bytes())
            .expect("parse_file"),
    );
    builder.build()
}

/// File paths of every `walks_tests` node that `run` calls.
fn callee_files_of_run(graph: &ZeroCopyGraph) -> Vec<String> {
    let pool = graph.string_pool.as_slice();
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == "run")
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == "walks_tests")
        .map(|e| {
            let file_idx = graph.nodes[e.target as usize].file_idx as usize;
            graph.files[file_idx].path.resolve(pool).to_string()
        })
        .collect()
}

#[test]
fn test_super_import_from_non_mod_file_resolves_to_parent_module_item() {
    let symbol_rs = r#"
use super::ImpactArgs;

fn run(args: &ImpactArgs) -> bool {
    args.walks_tests()
}
"#;
    let g = build(&[
        ("src/commands/impact/mod.rs", IMPACT_MOD),
        ("src/commands/walk/mod.rs", WALK_MOD),
        ("src/commands/impact/symbol.rs", symbol_rs),
    ]);
    assert_eq!(
        callee_files_of_run(&g),
        vec!["src/commands/impact/mod.rs".to_string()],
        "`super` from impact/symbol.rs is the impact module: exactly one edge, into impact/mod.rs"
    );
}

#[test]
fn test_super_brace_import_from_non_mod_file_resolves_to_parent_module_item() {
    let symbol_rs = r#"
use super::{ImpactArgs};

fn run(args: &ImpactArgs) -> bool {
    args.walks_tests()
}
"#;
    let g = build(&[
        ("src/commands/impact/mod.rs", IMPACT_MOD),
        ("src/commands/walk/mod.rs", WALK_MOD),
        ("src/commands/impact/symbol.rs", symbol_rs),
    ]);
    assert_eq!(
        callee_files_of_run(&g),
        vec!["src/commands/impact/mod.rs".to_string()]
    );
}

/// File paths of every `Item::make`/`helper` target that `caller` calls.
fn callee_files(graph: &ZeroCopyGraph, caller: &str, callee: &str) -> Vec<String> {
    let pool = graph.string_pool.as_slice();
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == callee)
        .map(|e| {
            let file_idx = graph.nodes[e.target as usize].file_idx as usize;
            graph.files[file_idx].path.resolve(pool).to_string()
        })
        .collect()
}

/// A crate at the repo root has repo-relative paths that start at `src/`,
/// with no `/src/` segment to anchor `crate::` on.
#[test]
fn test_crate_import_from_repo_root_crate_resolves_to_imported_module() {
    let g = build(&[
        ("src/lib.rs", "mod a;\nmod b;\nmod app;\n"),
        ("src/a.rs", "pub fn emit() {}\n"),
        ("src/b.rs", "pub fn emit() {}\n"),
        (
            "src/app.rs",
            "use crate::a::emit;\npub fn run() { emit(); }\n",
        ),
    ]);
    assert_eq!(
        callee_files(&g, "run", "emit"),
        vec!["src/a.rs".to_string()],
        "`use crate::a::emit` from src/app.rs is src/a.rs, not the same-named src/b.rs"
    );
}

/// A Rust module path names a Rust file: a same-stem TypeScript file next
/// to the module must not win the probe.
#[test]
fn test_crate_import_ignores_same_stem_file_of_another_language() {
    for root in ["", "crates/x/"] {
        let p = |f: &str| format!("{root}{f}");
        let g = build_with_ts(
            &[
                (p("src/lib.rs"), "mod a;\nmod app;\n"),
                (p("src/a.rs"), "pub fn emit() {}\n"),
                (
                    p("src/app.rs"),
                    "use crate::a::emit;\npub fn run() { emit(); }\n",
                ),
            ],
            &p("src/a.ts"),
            "export function emit() {}\n",
        );
        assert_eq!(
            callee_files(&g, "run", "emit"),
            vec![p("src/a.rs")],
            "root {root:?}"
        );
    }
}

/// `de::Error::custom` names `Error` through the module `de` (here the
/// external `serde::de`); an import of a different `Error` from
/// `crate::error` is not that qualifier. A second project `Error` keeps the
/// global lookup ambiguous, as in serde_json.
#[test]
fn test_path_qualifier_whose_module_prefix_differs_from_import_is_not_bound() {
    let error_rs = "pub struct Error;\nimpl Error {\n    pub fn custom() -> Error { Error }\n}\n";
    let app_rs = "use crate::error::Error;\nuse serde::de;\n\
        pub fn generic() { de::Error::custom(); }\n\
        pub fn direct() { Error::custom(); }\n\
        pub fn spelled() { crate::error::Error::custom(); }\n";
    let g = build(&[
        ("src/lib.rs", "mod error;\nmod other;\nmod app;\n"),
        ("src/error.rs", error_rs),
        ("src/other.rs", error_rs),
        ("src/app.rs", app_rs),
    ]);
    assert!(callee_files(&g, "generic", "custom").is_empty());
    assert_eq!(
        callee_files(&g, "direct", "custom"),
        vec!["src/error.rs".to_string()]
    );
    assert_eq!(
        callee_files(&g, "spelled", "custom"),
        vec!["src/error.rs".to_string()]
    );
}

#[test]
fn test_self_import_from_non_mod_file_resolves_to_stem_directory_child() {
    let item = "pub struct Item;\nimpl Item { pub fn make() -> Item { Item } }\n";
    let b_rs = "use self::c::Item;\nfn run() { Item::make(); }\n";
    let g = build(&[
        ("src/a/b.rs", b_rs),
        ("src/a/b/c.rs", item),
        ("src/a/c.rs", item),
    ]);
    assert_eq!(
        callee_files(&g, "run", "make"),
        vec!["src/a/b/c.rs".to_string()],
        "`self::c` from src/a/b.rs is src/a/b/c.rs, not the sibling src/a/c.rs"
    );
}

#[test]
fn test_self_import_from_cargo_test_root_resolves_to_sibling_module() {
    let it_rs = "mod support;\nuse self::support::helper;\nfn t() { helper(); }\n";
    let g = build(&[
        ("tests/it.rs", it_rs),
        ("tests/support.rs", "pub fn helper() {}\n"),
        ("src/other.rs", "pub fn helper() {}\n"),
    ]);
    assert_eq!(
        callee_files(&g, "t", "helper"),
        vec!["tests/support.rs".to_string()],
        "tests/it.rs is a crate root: `self::support` is tests/support.rs"
    );
}

#[test]
fn test_self_import_from_custom_lib_path_root_falls_back_to_own_directory() {
    // `[lib] path = "src/api.rs"`: the root is not named lib.rs, so the
    // module's own child directory (src/api/) is probed first and is empty.
    let api_rs = "mod child;\nuse self::child::helper;\npub fn run() { helper(); }\n";
    let g = build(&[
        ("src/api.rs", api_rs),
        ("src/child.rs", "pub fn helper() {}\n"),
        ("src/other/child.rs", "pub fn helper() {}\n"),
    ]);
    assert_eq!(
        callee_files(&g, "run", "helper"),
        vec!["src/child.rs".to_string()],
        "with no src/api/child.rs, `self::child` falls back to src/child.rs"
    );
}
