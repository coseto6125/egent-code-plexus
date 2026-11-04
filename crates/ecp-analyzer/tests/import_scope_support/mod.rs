//! Shared harness for the `<lang>_import_scope.rs` fixtures.
//!
//! An import narrower than its file (a Rust `use` in an inline `mod`, a C#
//! `using` in one `namespace { }` block, a Python `import` in a function)
//! is visible in that scope only. Each fixture states the edge the
//! language's own scope rule gives, never the edge ecp builds today. A case
//! ecp gets wrong today is `#[ignore = "needs import scope: ..."]` until the
//! scope table of `docs/specs/contracts/C4-raw-records.md` lands.
//!
//! Every fixture is written to disk under a temp root, so a resolver step
//! that reads the tree (a Rust module tree, a PHP `composer.json`) sees it.
//! Every assertion reads the graph the parse + `GraphBuilder` pipeline built.
#![allow(dead_code)]

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind, RelType, ZeroCopyGraph};
use ecp_core::session::{MergedGraph, OverlayFileInput, OverlayView};
use std::path::Path;

/// One edge out of a caller: `(target file, target name, target line)`,
/// the line 1-based.
pub type Hit = (String, String, u32);

pub fn hit(file: &str, name: &str, line: u32) -> Hit {
    (file.to_string(), name.to_string(), line)
}

pub fn parse<P: LanguageProvider>(provider: &P, path: &str, src: &str) -> LocalGraph {
    provider
        .parse_file(Path::new(path), src.as_bytes())
        .unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

/// The full build of `sources` (parsed by `provider`) under `root`.
/// `aux` files (`Cargo.toml`, `composer.json`, `tsconfig.json`) are written
/// to disk and never parsed.
pub fn build_in<P: LanguageProvider>(
    root: &Path,
    provider: &P,
    sources: &[(&str, &str)],
    aux: &[(&str, &str)],
) -> ZeroCopyGraph {
    let write = |rel: &str, src: &str| {
        let disk = root.join(rel);
        std::fs::create_dir_all(disk.parent().expect("parent")).expect("mkdir");
        std::fs::write(&disk, src).expect("write fixture file");
    };
    for (rel, src) in aux {
        write(rel, src);
    }
    let mut builder = GraphBuilder::new().with_repo_root(root.to_path_buf());
    for (rel, src) in sources {
        write(rel, src);
        builder.add_graph(parse(provider, rel, src));
    }
    builder.build()
}

pub fn build<P: LanguageProvider>(
    provider: &P,
    sources: &[(&str, &str)],
    aux: &[(&str, &str)],
) -> ZeroCopyGraph {
    let tmp = tempfile::tempdir().expect("tempdir");
    build_in(tmp.path(), provider, sources, aux)
}

fn is_callable(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Function | NodeKind::Method)
}

/// Every `Calls` edge out of the callable named `caller`, sorted. Give each
/// fixture's callers distinct names.
pub fn calls(g: &ZeroCopyGraph, caller: &str) -> Vec<Hit> {
    let pool = g.string_pool.as_slice();
    let mut out: Vec<Hit> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| {
            let source = &g.nodes[e.source as usize];
            is_callable(source.kind) && source.name.resolve(pool) == caller
        })
        .map(|e| {
            let target = &g.nodes[e.target as usize];
            let file = if target.has_owning_file() {
                g.files[target.file_idx as usize]
                    .path
                    .resolve(pool)
                    .to_string()
            } else {
                String::new()
            };
            (
                file,
                target.name.resolve(pool).to_string(),
                target.span.0 + 1,
            )
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// `(source name, target file, target name, target line)` of every `Calls`
/// edge, sorted. The file keeps two namesakes on the same line of
/// different files (`b.h:g:2`, `c.h:g:2`) apart.
fn merged_calls(merged: &MergedGraph<'_>) -> Vec<(String, String, String, u32)> {
    let mut out: Vec<_> = merged
        .all_edges()
        .filter(|e| e.rel_type() == RelType::Calls)
        .filter_map(|e| {
            let (s, t) = (merged.node(e.source)?, merged.node(e.target)?);
            Some((
                s.name(merged).to_string(),
                t.file_path(merged).unwrap_or_default().to_string(),
                t.name(merged).to_string(),
                t.start_line(),
            ))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The full build of `sources`, and the overlay view of `dirty` (one of
/// them, unchanged) over it, give the same `Calls` edges. Equal results can
/// still be equally wrong: pair this with a target assertion.
pub fn assert_overlay_matches_full<P: LanguageProvider>(
    provider: &P,
    sources: &[(&str, &str)],
    aux: &[(&str, &str)],
    dirty: &str,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let full = build_in(tmp.path(), provider, sources, aux);
    let src = sources
        .iter()
        .find(|(path, _)| *path == dirty)
        .map(|(_, src)| *src)
        .expect("setup: the dirty file is one of the sources");
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&full).expect("archive graph");
    let base =
        rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).expect("access graph");
    let view = OverlayView::build(base, &[OverlayFileInput::from(parse(provider, dirty, src))])
        .expect("a view");
    assert_eq!(
        merged_calls(&MergedGraph::new(base, Some(&view))),
        merged_calls(&MergedGraph::new(base, None)),
        "Calls: overlay of {dirty} vs full build"
    );
}
