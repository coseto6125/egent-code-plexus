use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
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
