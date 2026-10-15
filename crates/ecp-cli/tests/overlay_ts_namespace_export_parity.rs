//! Recorded divergence (FU-2026-10-04-14079e7e2d8b): the full index binds a
//! TS / JS import only to a declaration the module exports, and the overlay
//! does not. Export status lives in the analyzer's in-memory symbol table;
//! `graph.bin` carries no export flag, so the overlay cannot tell a clean
//! module's private `f` from an exported one and keeps taking the module's
//! first `f`. These tests pin both sides, so a change to either shows up.

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType, ZeroCopyGraph};
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::view::{OverlayFileInput, OverlaySymbol, OverlayView};
use std::path::Path;

/// `(target file, callee, owner, confidence in percent)`.
type Target = (String, String, String, u32);

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
                n.owner_class.resolve(pool).into(),
                (e.confidence * 100.0).round() as u32,
            )
        })
        .collect();
    result.sort();
    result.dedup();
    result
}

/// Calls from `caller` after `dirty` is applied: `(full reindex, overlay)`.
fn reindex_and_overlay(
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    dirty: &[(&str, &str)],
    caller: &str,
) -> (Vec<Target>, Vec<Target>) {
    let tmp = tempfile::tempdir().unwrap();
    let build = |edits: &[(&str, &str)]| {
        let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
        for &(path, original) in files {
            let source = edits
                .iter()
                .find(|(p, _)| *p == path)
                .map_or(original, |(_, s)| *s);
            let disk = tmp.path().join(path);
            std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
            std::fs::write(disk, source).unwrap();
            builder.add_graph(
                provider
                    .parse_file(Path::new(path), source.as_bytes())
                    .unwrap(),
            );
        }
        builder.build()
    };
    let base = build(&[]);
    let full = build(dirty);
    let inputs: Vec<_> = dirty
        .iter()
        .map(|(path, source)| {
            input(
                provider
                    .parse_file(Path::new(path), source.as_bytes())
                    .unwrap(),
            )
        })
        .collect();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&base).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let view = OverlayView::build(archived, &inputs).unwrap();
    let merged = MergedGraph::new(archived, Some(&view));
    let mut overlay: Vec<Target> = merged
        .all_edges()
        .filter(|e| {
            e.rel_type() == RelType::Calls && merged.node(e.source).unwrap().name(&merged) == caller
        })
        .map(|e| {
            let n = merged.node(e.target).unwrap();
            (
                n.file_path(&merged).unwrap().into(),
                n.name(&merged).into(),
                n.owner_class(&merged).unwrap_or_default().into(),
                (e.confidence() * 100.0).round() as u32,
            )
        })
        .collect();
    overlay.sort();
    overlay.dedup();
    (reindex_targets(&full, caller), overlay)
}

/// The FU repro, edited into a dirty importer. The reindex binds no edge;
/// the overlay still binds the clean module's private `f` at the import
/// tier. Closing the gap needs export status in `graph.bin` (or in the
/// overlay's inputs for clean files), a format change outside this fix.
#[test]
fn test_overlay_namespace_member_shadowed_by_export_alias_diverges_from_reindex() {
    let ts = TypeScriptProvider::new().unwrap();
    let js = JavaScriptProvider::new().unwrap();
    for (provider, ext) in [
        (&ts as &dyn LanguageProvider, "ts"),
        (&js as &dyn LanguageProvider, "js"),
    ] {
        let module = format!("a.{ext}");
        let importer = format!("b.{ext}");
        let (reindex, overlay) = reindex_and_overlay(
            provider,
            &[
                (
                    &module,
                    "function f() {}\nfunction g() {}\nexport { g as f };\n",
                ),
                (&importer, "import * as ns from './a';\nfunction use() {}\n"),
            ],
            &[(
                &importer,
                "import * as ns from './a';\nfunction use() { ns.f(); }\n",
            )],
            "use",
        );
        assert_eq!(reindex, Vec::<Target>::new(), "{ext}: reindex");
        assert_eq!(
            overlay,
            vec![(module.clone(), "f".to_string(), String::new(), 95)],
            "{ext}: overlay (recorded divergence)"
        );
    }
}
