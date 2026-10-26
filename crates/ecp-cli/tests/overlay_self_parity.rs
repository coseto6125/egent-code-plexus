//! Overlay self-parity: an overlay that marks files dirty without changing
//! them must give the `Calls` edges the index gave. Any difference is a rule
//! the two sides do not share.
//!
//! What the fixture does not cover: a crate-head path through a `pub use`
//! (`kit::build()`). `graph.bin` keeps no import visibility, so the overlay
//! cannot follow the re-export the index follows
//! (FU-2026-10-26-fb9b443e70ba).

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_cli::config_parser::parse_configs;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType, ZeroCopyGraph};
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::view::{OverlayFileInput, OverlayView};
use ecp_core::session::OVERLAY_EDGE_REASON;
use std::path::Path;

/// One `Calls` edge by identity: `(caller file, caller, callee file,
/// callee, callee line, confidence, reason)`. The overlay's
/// uncommitted-edit marker reads as the index's plain `call`.
type Call = (String, String, String, String, u32, f32, String);

fn is_rust(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|e| e == "rs")
}

fn parse(path: &str, source: &str) -> ecp_core::analyzer::types::LocalGraph {
    ecp_cli::reanalyze::pipeline()
        .parse_file_raw(Path::new(path), source.as_bytes())
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Index `files` from a temporary repo root that holds every one of them,
/// with the config aliases `ecp admin index` passes.
fn index(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let root = tempfile::tempdir().expect("tempdir");
    for &(rel, source) in files {
        let path = root.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("a file has a directory")).unwrap();
        std::fs::write(&path, source).unwrap();
    }
    let mut builder = GraphBuilder::new()
        .with_path_aliases(parse_configs(root.path()))
        .with_repo_root(root.path().to_path_buf());
    for &(rel, source) in files.iter().filter(|(rel, _)| is_rust(rel)) {
        builder.add_graph(parse(rel, source));
    }
    builder.build()
}

/// Every `Calls` edge of `merged`, sorted. No dedup: a base edge the
/// overlay fails to mask shows twice.
fn calls<'g>(merged: &MergedGraph<'g>, graph: &'g ArchivedZeroCopyGraph) -> Vec<Call> {
    let mut edges: Vec<Call> = merged
        .all_edges()
        .filter(|edge| edge.rel_type() == RelType::Calls)
        .filter_map(|edge| {
            let source = merged.node(edge.source)?;
            let callee = merged.node(edge.target)?;
            let reason = match edge.reason(graph) {
                OVERLAY_EDGE_REASON => "call",
                reason => reason,
            };
            Some((
                source.file_path(merged)?.to_string(),
                source.name(merged).to_string(),
                callee.file_path(merged)?.to_string(),
                callee.name(merged).to_string(),
                callee.start_line(),
                edge.confidence(),
                reason.to_string(),
            ))
        })
        .collect();
    edges.sort_by(|a, b| a.partial_cmp(b).expect("a confidence is never NaN"));
    edges
}

/// Three crates: `app` calls into `engine` and `kit` by crate head, through
/// `crate::`, and through a `use` of `kit`'s `pub use` re-export; `engine`
/// nests `net/mod.rs` and `net/tcp.rs` and calls through `self::` and
/// `super::super::`; `kit` calls through `super::` and `crate::`.
const WORKSPACE: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/app\", \"crates/engine\", \"crates/kit\"]\n",
    ),
    (
        "crates/app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nengine = { path = \"../engine\" }\nkit = { path = \"../kit\" }\n",
    ),
    (
        "crates/app/src/lib.rs",
        "mod cli;\n\nuse kit::build;\n\npub fn run() -> u32 {\n    engine::start() + build() + crate::cli::parse() + kit::version()\n}\n",
    ),
    (
        "crates/app/src/cli.rs",
        "pub fn parse() -> u32 {\n    engine::net::tcp::dial()\n}\n",
    ),
    (
        "crates/engine/Cargo.toml",
        "[package]\nname = \"engine\"\nversion = \"0.1.0\"\n\n[dependencies]\nkit = { path = \"../kit\" }\n",
    ),
    (
        "crates/engine/src/lib.rs",
        "pub mod net;\n\npub fn start() -> u32 {\n    self::net::open() + kit::text::upper()\n}\n\nfn helper() -> u32 {\n    2\n}\n",
    ),
    (
        "crates/engine/src/net/mod.rs",
        "pub mod tcp;\n\npub fn open() -> u32 {\n    tcp::dial()\n}\n",
    ),
    (
        "crates/engine/src/net/tcp.rs",
        "pub fn dial() -> u32 {\n    super::super::helper()\n}\n",
    ),
    (
        "crates/kit/Cargo.toml",
        "[package]\nname = \"kit\"\nversion = \"0.1.0\"\n",
    ),
    (
        "crates/kit/src/lib.rs",
        "pub mod text;\nmod make;\n\npub use make::build;\n\npub fn version() -> u32 {\n    1\n}\n",
    ),
    (
        "crates/kit/src/text.rs",
        "pub fn upper() -> u32 {\n    super::version()\n}\n",
    ),
    (
        "crates/kit/src/make.rs",
        "pub fn build() -> u32 {\n    crate::text::upper()\n}\n",
    ),
];

/// The one edge the two sides bind differently today, as (caller, callee,
/// base confidence, overlay confidence). `use kit::build; build()` reaches
/// `kit`'s `pub use make::build`: the index binds it at the global tier, the
/// overlay at the import tier; 0.16.4 does the same
/// (FU-2026-10-26-6d659c216941). The check fails when this edge agrees, or
/// when any other edge differs.
const KNOWN_DIVERGENCE: (&str, &str, f32, f32) = ("run", "build", 0.7, 0.95);

#[test]
fn test_overlay_self_parity_rust_workspace_dirty_unchanged_files_equal_base() {
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&index(WORKSPACE)).unwrap();
    let base = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let expected = calls(&MergedGraph::new(base, None), base);
    // The fixture proves nothing unless the index binds the module-tree
    // paths it holds: each of these calls has one edge in the base.
    for (caller, callee) in [
        ("start", "open"),
        ("start", "upper"),
        ("dial", "helper"),
        ("upper", "version"),
        ("build", "upper"),
        ("parse", "dial"),
        ("run", "start"),
        ("run", "parse"),
        ("run", "version"),
    ] {
        let found = expected
            .iter()
            .filter(|call| call.1 == caller && call.3 == callee)
            .count();
        assert_eq!(found, 1, "{caller} -> {callee} in the base: {expected:#?}");
    }

    let inputs: Vec<OverlayFileInput> = WORKSPACE
        .iter()
        .filter(|(rel, _)| is_rust(rel))
        .map(|&(rel, source)| parse(rel, source).into())
        .collect();
    let view = OverlayView::build(base, &inputs).expect("dirty files make a view");
    let merged = MergedGraph::new(base, Some(&view));
    let (caller, callee, base_conf, overlay_conf) = KNOWN_DIVERGENCE;
    let mut want = expected.clone();
    let diverging = want
        .iter_mut()
        .filter(|call| call.1 == caller && call.3 == callee && call.5 == base_conf)
        .map(|call| call.5 = overlay_conf)
        .count();
    assert_eq!(
        diverging, 1,
        "the known divergence is in the base: {expected:#?}"
    );
    want.sort_by(|a, b| a.partial_cmp(b).expect("a confidence is never NaN"));
    assert_eq!(calls(&merged, base), want);
}
