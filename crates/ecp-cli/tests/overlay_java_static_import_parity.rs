//! FU-2026-10-05-e838f64395a2: the overlay's import tier (`session/view/resolution.rs`)
//! pins a Java import's member owner from the import path exactly as the
//! index does (`SymbolTable::lookup_member_in_file`). Each fixture asserts
//! the full reindex and the merged overlay give the same Calls targets.
use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType, ZeroCopyGraph};
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::view::{OverlayFileInput, OverlayView};
use std::path::Path;

/// (file, name, owner, confidence in hundredths).
type Target = (String, String, String, u32);

fn input(local: LocalGraph) -> OverlayFileInput {
    local.into()
}

fn build(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let provider = JavaProvider::new().unwrap();
    let mut builder = GraphBuilder::new();
    for &(path, source) in files {
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
    let mut result: Vec<_> = graph
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
    result
}

/// `files` is the indexed state; `dirty` replaces or adds files. The full
/// reindex of the edited tree and the overlay over the old index must both
/// give `expected` for `caller`.
fn parity(caller: &str, files: &[(&str, &str)], dirty: &[(&str, &str)], expected: &[Target]) {
    let base = build(files);
    let mut edited: Vec<(&str, &str)> = files
        .iter()
        .filter(|(path, _)| !dirty.iter().any(|(d, _)| d == path))
        .copied()
        .collect();
    edited.extend_from_slice(dirty);
    assert_eq!(
        reindex_targets(&build(&edited), caller),
        expected,
        "full reindex: {dirty:?}"
    );
    let provider = JavaProvider::new().unwrap();
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
    // Every view a test builds must survive the session view cache unchanged.
    assert_eq!(
        OverlayView::from_cache_bytes(&view.to_cache_bytes(), archived.nodes.len()).as_ref(),
        Some(&view),
        "view cache round trip"
    );
    let merged = MergedGraph::new(archived, Some(&view));
    let mut actual: Vec<Target> = merged
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
    actual.sort();
    assert_eq!(actual, expected, "merged overlay: {dirty:?}");
}

fn target(file: &str, name: &str, owner: &str, confidence: u32) -> Target {
    (file.into(), name.into(), owner.into(), confidence)
}

const UTIL_AFTER_OTHER: (&str, &str) = (
    "r/Util.java",
    "package r;\nclass Other { static void run(){} }\npublic class Util { public static void run(){} }\n",
);

const SAME_NAMED_NESTED: &str = "package h;\nclass Outer2 { static class Helper { static void run(){} } }\npublic class Outer1 { static class Helper { static void run(){} } }\n";

#[test]
fn test_overlay_static_import_owner_declared_second_matches_reindex() {
    parity(
        "go",
        &[
            UTIL_AFTER_OTHER,
            ("q/C2.java", "package q;\nclass C2 { void go() {} }\n"),
        ],
        &[(
            "q/C2.java",
            "package q;\nimport static r.Util.run;\nclass C2 { void go() { run(); } }\n",
        )],
        &[target("r/Util.java", "run", "Util", 95)],
    );
}

/// The module file is clean: the overlay reads its types from the index.
#[test]
fn test_overlay_nested_class_import_same_named_owner_matches_reindex() {
    parity(
        "go3",
        &[
            ("h/Outer1.java", SAME_NAMED_NESTED),
            ("q/C3.java", "package q;\nclass C3 { void go3() {} }\n"),
        ],
        &[(
            "q/C3.java",
            "package q;\nimport h.Outer1.Helper;\nclass C3 { void go3() { Helper.run(); } }\n",
        )],
        &[],
    );
}

/// The module file is dirty: the overlay reads its types from the fresh
/// parse, never from the stale index nodes.
#[test]
fn test_overlay_nested_class_import_dirty_module_matches_reindex() {
    parity(
        "go3",
        &[
            (
                "h/Outer1.java",
                "package h;\npublic class Outer1 { static class Helper { static void run(){} } }\n",
            ),
            ("q/C3.java", "package q;\nclass C3 { void go3() {} }\n"),
        ],
        &[
            ("h/Outer1.java", SAME_NAMED_NESTED),
            (
                "q/C3.java",
                "package q;\nimport h.Outer1.Helper;\nclass C3 { void go3() { Helper.run(); } }\n",
            ),
        ],
        &[],
    );
}
