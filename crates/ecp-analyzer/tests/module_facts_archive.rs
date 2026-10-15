//! The graph archive keeps each Python / TS / JS file's imports in source
//! order, the alias-vs-none distinction included, plus each ES module's
//! flag, export aliases and exported declarations: what the session overlay
//! reads to follow a clean module's re-export (FU-2026-10-15-61d656576fc8).

use ecp_analyzer::python::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::reexport::ImportRef;
use ecp_core::analyzer::types::{BindingKind, LocalGraph};
use ecp_core::graph::ArchivedZeroCopyGraph;
use std::path::Path;

const FILES: &[(&str, &str)] = &[
    (
        "pkg/__init__.py",
        "from .core import compute as c\nfrom .core import *\n",
    ),
    ("pkg/core.py", "def compute():\n    pass\n"),
    (
        "web/a.ts",
        "import { x } from './b';\nfunction f() {}\nfunction g() {}\nexport { g as f };\n",
    ),
    ("web/b.ts", "export function x() {}\nfunction h() {}\n"),
    ("web/c.ts", "function k() {}\n"),
    (
        "web/barrel.ts",
        "import d from './b';\nimport { x } from './b';\nexport { x, d };\nexport { y } from './c';\n",
    ),
    (
        "web/page.ts",
        "export const config = 1;\nfunction h() {}\nexport default wrap(h);\n",
    ),
    ("web/types.d.ts", "export function t(): void;\n"),
];

fn parse(path: &str, source: &str) -> LocalGraph {
    let python = PythonProvider::new().unwrap();
    let typescript = TypeScriptProvider::new().unwrap();
    let provider: &dyn LanguageProvider = if path.ends_with(".py") {
        &python
    } else {
        &typescript
    };
    provider
        .parse_file(Path::new(path), source.as_bytes())
        .unwrap()
}

#[test]
fn test_build_module_facts_mirror_each_files_parse() {
    let locals: Vec<LocalGraph> = FILES
        .iter()
        .map(|(path, source)| parse(path, source))
        .collect();
    let mut builder = GraphBuilder::new();
    for local in locals.clone() {
        builder.add_graph(local);
    }
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&builder.build()).unwrap();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    assert!(graph.has_module_facts());
    let pool = graph.string_pool.as_slice();
    for local in &locals {
        let path = local.file_path.to_string_lossy();
        let file = graph.file_idx_of(&path).expect("every file is indexed");
        let archived: Vec<ImportRef<'_>> = graph
            .file_imports(file)
            .expect("facts are present")
            .iter()
            .map(|row| row.to_ref(pool))
            .collect();
        let parsed: Vec<ImportRef<'_>> = local.imports.iter().map(ImportRef::from).collect();
        assert_eq!(archived, parsed, "{path}");
    }
    let init = graph.file_idx_of("pkg/__init__.py").unwrap();
    let aliases: Vec<Option<&str>> = graph
        .file_imports(init)
        .unwrap()
        .iter()
        .map(|row| row.to_ref(pool).alias)
        .collect();
    assert_eq!(aliases, [Some("c"), None], "order and alias-vs-none kept");

    let es_module = |path: &str| graph.is_es_module_file(graph.file_idx_of(path).unwrap());
    assert!(es_module("web/a.ts"), "an export alias makes an ES module");
    assert!(
        es_module("web/b.ts"),
        "an exported declaration makes an ES module"
    );
    assert!(!es_module("web/c.ts"), "a script exports nothing");
    assert!(!es_module("web/types.d.ts"), "a .d.ts proves nothing");
    assert!(!es_module("pkg/core.py"));

    let a = graph.file_idx_of("web/a.ts").unwrap();
    let alias_rows: Vec<(&str, &str)> = graph
        .file_export_aliases(a)
        .unwrap()
        .iter()
        .map(|row| (row.exported.resolve(pool), row.local.resolve(pool)))
        .collect();
    assert!(alias_rows.contains(&("f", "g")), "{alias_rows:?}");
    let exported = |name: &str| {
        graph
            .nodes_by_name(name)
            .filter(|&idx| graph.nodes[idx as usize].file_idx.to_native() == a)
            .any(|idx| graph.is_exported_node(idx))
    };
    assert!(exported("g") && !exported("f"));

    // What the barrel exports, the binding kind the overlay's re-export
    // rule reads.
    let barrel = graph.file_idx_of("web/barrel.ts").unwrap();
    let kinds: Vec<(&str, Option<BindingKind>)> = graph
        .file_imports(barrel)
        .unwrap()
        .iter()
        .map(|row| {
            let import = row.to_ref(pool);
            (import.imported_name, import.binding_kind)
        })
        .collect();
    for kind in [
        ("d", Some(BindingKind::EsDefaultReexport)),
        ("x", Some(BindingKind::EsReexport)),
        ("y", Some(BindingKind::EsReexport)),
    ] {
        assert!(kinds.contains(&kind), "{kind:?} in {kinds:?}");
    }
    // An expression default keeps its empty local through the archive.
    let page = graph.file_idx_of("web/page.ts").unwrap();
    let page_aliases: Vec<(&str, &str)> = graph
        .file_export_aliases(page)
        .unwrap()
        .iter()
        .map(|row| (row.exported.resolve(pool), row.local.resolve(pool)))
        .collect();
    assert_eq!(page_aliases, [("config", "config"), ("default", "")]);
}
