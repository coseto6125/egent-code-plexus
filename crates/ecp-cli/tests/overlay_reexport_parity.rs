//! Records a known divergence between the full reindex and the session
//! overlay (FU-2026-10-05-0ee319339782).
//!
//! The reindex follows a re-export one hop: a call through
//! `pkg/__init__.py`'s `from .core import compute`, or through a TS barrel's
//! `export * from './impl'`, binds at the import tier (0.95). The overlay
//! cannot mirror that: the hop reads the re-exporting module's own imports,
//! and graph.bin keeps no `RawImport` for a clean file, only its resolved
//! `Imports` edges. So a dirty caller of a clean barrel keeps the overlay's
//! pre-fix answer: the global tier, which drops an ambiguous name.
//!
//! When the overlay gains the hop, these tests go red: replace each
//! `overlay_expected` with the reindex row and fold them into
//! `overlay_resolver_parity.rs`.

use ecp_analyzer::python::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType, ZeroCopyGraph};
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::view::{OverlayFileInput, OverlaySymbol, OverlayView};
use std::path::Path;

type Target = (String, String, u32);

fn input(local: LocalGraph) -> OverlayFileInput {
    OverlayFileInput {
        rel_path: local.file_path.to_string_lossy().into_owned(),
        imports: local.imports,
        symbols: local
            .nodes
            .into_iter()
            .map(|n| OverlaySymbol {
                name: n.name,
                kind: n.kind,
                owner_class: n.owner_class,
                start_line: n.span.0 + 1,
                end_line: n.span.2 + 1,
                start_column: n.span.1,
                end_column: n.span.3,
                calls: n.calls,
            })
            .collect(),
    }
}

fn build(provider: &dyn LanguageProvider, files: &[(&str, &str)]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for (path, source) in files {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), source.as_bytes())
                .unwrap(),
        );
    }
    builder.build()
}

fn reindex_targets(graph: &ZeroCopyGraph, caller: &str) -> Vec<Target> {
    let pool = &graph.string_pool;
    let mut result: Vec<Target> = graph
        .edges
        .iter()
        .filter(|e| {
            e.rel_type == RelType::Calls
                && graph.nodes[e.source as usize].name.resolve(pool) == caller
        })
        .map(|e| {
            let n = &graph.nodes[e.target as usize];
            (
                graph.files[n.file_idx as usize].path.resolve(pool).into(),
                n.name.resolve(pool).into(),
                (e.confidence * 100.0).round() as u32,
            )
        })
        .collect();
    result.sort();
    result
}

/// `files` is the committed tree; `dirty` replaces `caller`'s file with an
/// edit that adds the call.
fn check(
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    dirty: (&str, &str),
    reindex_expected: &[Target],
    overlay_expected: &[Target],
) {
    let base = build(provider, files);
    let edited: Vec<_> = files
        .iter()
        .map(|&(path, source)| {
            if path == dirty.0 {
                dirty
            } else {
                (path, source)
            }
        })
        .collect();
    assert_eq!(
        reindex_targets(&build(provider, &edited), "go"),
        reindex_expected,
        "the reindex must follow the re-export"
    );
    let inputs = [input(
        provider
            .parse_file(Path::new(dirty.0), dirty.1.as_bytes())
            .unwrap(),
    )];
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&base).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let view = OverlayView::build(archived, &inputs).unwrap();
    let merged = MergedGraph::new(archived, Some(&view));
    let mut actual: Vec<Target> = merged
        .all_edges()
        .filter(|e| {
            e.rel_type() == RelType::Calls && merged.node(e.source).unwrap().name(&merged) == "go"
        })
        .map(|e| {
            let n = merged.node(e.target).unwrap();
            (
                n.file_path(&merged).unwrap().into(),
                n.name(&merged).into(),
                (e.confidence() * 100.0).round() as u32,
            )
        })
        .collect();
    actual.sort();
    actual.dedup();
    assert_eq!(
        actual, overlay_expected,
        "overlay Calls from the dirty caller"
    );
}

#[test]
fn test_overlay_python_package_reexport_diverges_from_reindex() {
    let files = [
        ("pkg/__init__.py", "from .core import compute\n"),
        ("pkg/core.py", "def compute():\n    return 1\n"),
        ("pkg/other.py", "def compute():\n    return 2\n"),
        ("app.py", "from pkg import compute\n\ndef go():\n    pass\n"),
    ];
    check(
        &PythonProvider::new().unwrap(),
        &files,
        (
            "app.py",
            "from pkg import compute\n\ndef go():\n    return compute()\n",
        ),
        &[("pkg/core.py".into(), "compute".into(), 95)],
        &[],
    );
}

#[test]
fn test_overlay_typescript_star_reexport_diverges_from_reindex() {
    let files = [
        ("ts/index.ts", "export * from './impl';\n"),
        ("ts/impl.ts", "export function render() { return 1; }\n"),
        ("ts/other.ts", "export function render() { return 2; }\n"),
        (
            "ts/app.ts",
            "import { render } from './index';\nexport function go() { return 0; }\n",
        ),
    ];
    check(
        &TypeScriptProvider::new().unwrap(),
        &files,
        (
            "ts/app.ts",
            "import { render } from './index';\nexport function go() { return render(); }\n",
        ),
        &[("ts/impl.ts".into(), "render".into(), 95)],
        &[],
    );
}
