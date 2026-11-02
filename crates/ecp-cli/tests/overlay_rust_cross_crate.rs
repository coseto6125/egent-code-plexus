//! The Rust crate table in `graph.bin` (contract C2, Cargo rows) and the
//! overlay's `<crate_name>::path::f()` heads that read it.
//!
//! A full index binds a call into another workspace crate through its
//! module tree (Tier 3.5). Both sides run one rule,
//! `ladder::module_tree_item`: the index over the tree it reads from disk,
//! the overlay over the crate name → entry file rows the index stored and
//! the stored `mod` declarations. Each test indexes a workspace from a
//! temporary root that holds every file, as `ecp admin index` does, then
//! builds the overlay of a dirty caller over that index.
//!
//! `graph.bin` keeps no import visibility, so the overlay reads a module's
//! `pub use` rows from its text, as the index does: the view these tests
//! build reads each file of the edited tree, as the engine reads the
//! worktree.
//!
//! What the fixtures do not cover: inline `mod util { .. }` modules, and
//! `#[path]` module files beyond one guard: the overlay reads no `pub use`
//! rows of a module whose path holds one. The overlay walks file-backed
//! `mod` declarations only.
//!
//! The `test_check_rust_*` cases at the end hold those gaps as known
//! differences against a fresh index in
//! `tests/baselines/strict_parity/<scenario>.txt` (`common/strict_parity.rs`):
//! the repros are non-empty until P3 lands, and the guard stays empty.

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_cli::config_parser::parse_configs;
use ecp_core::analyzer::outcome::Tier;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::workspace::{NameRank, WorkspaceFacts};
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType, ZeroCopyGraph};
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::view::{OverlayFileInput, OverlayView};
use ecp_core::session::OVERLAY_EDGE_REASON;
use std::path::Path;
use strict_parity::{check, Edit, Scenario};

/// One `Calls` edge out of a named caller, by identity, with its reason.
/// The overlay's uncommitted-edit marker reads as the index's plain `call`.
#[derive(Debug, PartialEq, PartialOrd)]
struct Call {
    callee_file: String,
    callee: String,
    callee_line: u32,
    confidence: f32,
    reason: String,
}

fn owned(files: &[(&str, &str)]) -> Vec<(String, String)> {
    files
        .iter()
        .map(|&(path, source)| (path.to_string(), source.to_string()))
        .collect()
}

fn parse(path: &str, source: &str) -> LocalGraph {
    ecp_cli::reanalyze::pipeline()
        .parse_file_raw(Path::new(path), source.as_bytes())
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn is_source(path: &str) -> bool {
    matches!(
        Path::new(path).extension().and_then(|e| e.to_str()),
        Some("rs" | "py")
    )
}

/// Index `files` from a temporary repo root that holds every one of them,
/// with the config aliases `ecp admin index` passes.
fn index(files: &[(String, String)]) -> ZeroCopyGraph {
    let root = tempfile::tempdir().expect("tempdir");
    for (rel, source) in files {
        let path = root.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("a file has a directory")).unwrap();
        std::fs::write(&path, source).unwrap();
    }
    let mut builder = GraphBuilder::new()
        .with_path_aliases(parse_configs(root.path()))
        .with_repo_root(root.path().to_path_buf());
    for (rel, source) in files {
        if is_source(rel) {
            builder.add_graph(parse(rel, source));
        }
    }
    builder.build()
}

/// The `Calls` edges out of every `caller` declared in `caller_file`,
/// sorted. No dedup: a base edge the overlay fails to mask shows twice.
fn calls<'g>(
    merged: &MergedGraph<'g>,
    graph: &'g ArchivedZeroCopyGraph,
    caller_file: &str,
    caller: &str,
) -> Vec<Call> {
    let mut edges: Vec<Call> = merged
        .all_edges()
        .filter(|edge| edge.rel_type() == RelType::Calls)
        .filter_map(|edge| {
            let source = merged.node(edge.source)?;
            if source.file_path(merged)? != caller_file || source.name(merged) != caller {
                return None;
            }
            let callee = merged.node(edge.target)?;
            let reason = match edge.reason(graph) {
                OVERLAY_EDGE_REASON => "call",
                reason => reason,
            };
            Some(Call {
                callee_file: callee.file_path(merged)?.to_string(),
                callee: callee.name(merged).to_string(),
                callee_line: callee.start_line(),
                confidence: edge.confidence(),
                reason: reason.to_string(),
            })
        })
        .collect();
    edges.sort_by(|a, b| a.partial_cmp(b).expect("a confidence is never NaN"));
    edges
}

/// The archived graph of `graph`, through the bytes `graph.bin` holds.
fn archive(graph: &ZeroCopyGraph) -> rkyv::util::AlignedVec {
    rkyv::to_bytes::<rkyv::rancor::Error>(graph).unwrap()
}

fn access(bytes: &[u8]) -> &ArchivedZeroCopyGraph {
    rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).unwrap()
}

/// The calls out of `callers` in `caller_file` after `caller_file` becomes
/// `edited`: from a full reindex of the edited tree, and from the overlay of
/// the edit over the index of `files`, which reads module text from the
/// edited tree.
fn reindex_and_overlay(
    files: &[(String, String)],
    caller_file: &str,
    edited: &str,
    callers: &[&str],
) -> (Vec<Call>, Vec<Call>) {
    reindex_and_overlay_of(files, caller_file, edited, caller_file, callers)
}

/// [`reindex_and_overlay`] with the edit in `dirty`, which may be another
/// file than `caller_file`. The overlay reads `dirty` from its fresh parse
/// and re-resolves the unchanged `caller_file` against it, as the session's
/// invalidation closure hands a dependent to the view.
fn reindex_and_overlay_of(
    files: &[(String, String)],
    dirty: &str,
    edited: &str,
    caller_file: &str,
    callers: &[&str],
) -> (Vec<Call>, Vec<Call>) {
    let edited_tree: Vec<(String, String)> = files
        .iter()
        .map(|(path, source)| {
            let source = if path == dirty { edited } else { source };
            (path.clone(), source.to_string())
        })
        .collect();
    let full_bytes = archive(&index(&edited_tree));
    let full = access(&full_bytes);
    let full_merged = MergedGraph::new(full, None);
    let reindex = callers
        .iter()
        .flat_map(|caller| calls(&full_merged, full, caller_file, caller))
        .collect();

    let base_bytes = archive(&index(files));
    let base = access(&base_bytes);
    let mut inputs: Vec<OverlayFileInput> = vec![parse(dirty, edited).into()];
    if caller_file != dirty {
        let (_, source) = files
            .iter()
            .find(|(path, _)| path == caller_file)
            .expect("the caller's file is a fixture file");
        inputs.push(parse(caller_file, source).into());
    }
    let sources = |rel: &str| {
        edited_tree
            .iter()
            .find(|(path, _)| path == rel)
            .map(|(_, source)| source.clone())
    };
    let view =
        OverlayView::build_with_sources(base, &inputs, &sources).expect("an edit makes a view");
    assert_eq!(
        OverlayView::from_cache_bytes(&view.to_cache_bytes(), base.nodes.len()).as_ref(),
        Some(&view),
        "view cache round trip"
    );
    let merged = MergedGraph::new(base, Some(&view));
    let overlay = callers
        .iter()
        .flat_map(|caller| calls(&merged, base, caller_file, caller))
        .collect();
    (reindex, overlay)
}

/// `source` with a comment line after its first line: the call survives,
/// every line below moves.
fn one_line_edit(source: &str) -> String {
    let (first, rest) = source.split_once('\n').expect("a multi-line fixture");
    format!("{first}\n// edited\n{rest}")
}

/// The reindex binds each `(callee_file, callee)` once, through the module
/// tree; otherwise the fixture proves nothing about the overlay.
fn assert_module_tree_binds(reindex: &[Call], expected: &[(&str, &str)], label: &str) {
    let bound: Vec<(&str, &str)> = reindex
        .iter()
        .filter(|call| call.confidence == Tier::ModuleTree.confidence())
        .map(|call| (call.callee_file.as_str(), call.callee.as_str()))
        .collect();
    assert_eq!(
        bound, expected,
        "{label}: the reindex does not bind through the module tree: {reindex:#?}"
    );
}

const CALLER: &str = "crates/a/src/lib.rs";

/// Workspace `crates/a` and `crates/b`. The caller's own crate declares a
/// `util::f` too, so only the crate head tells the two `f` apart.
fn two_crates(caller_source: &str, b_manifest: &str) -> Vec<(String, String)> {
    let mut files = owned(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
        ),
        (
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
        ),
        ("crates/a/src/util.rs", "pub fn f() {}\n"),
        ("crates/b/Cargo.toml", b_manifest),
        ("crates/b/src/lib.rs", "pub mod util;\n"),
        ("crates/b/src/util.rs", "pub fn f() {}\n\npub fn g() {}\n"),
    ]);
    files.push((CALLER.to_string(), caller_source.to_string()));
    files
}

#[test]
fn test_overlay_view_dirty_rust_file_cross_crate_call_keeps_edge() {
    let caller = "pub mod util;\n\npub fn t() {\n    b::util::f();\n}\n";
    let files = two_crates(caller, "[package]\nname = \"b\"\nversion = \"0.1.0\"\n");

    let graph = index(&files);
    let stored: Vec<(&str, Vec<&str>)> = graph
        .workspace
        .scopes
        .iter()
        .map(|scope| {
            (
                scope.root.as_str(),
                scope.entry_files.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        stored,
        vec![
            ("crates/a", vec!["crates/a/src/lib.rs"]),
            ("crates/b", vec!["crates/b/src/lib.rs"]),
        ],
        "one scope per member, its lib entry first"
    );

    for (label, edited) in [
        ("touched", caller.to_string()),
        ("one-line edit", one_line_edit(caller)),
    ] {
        let (reindex, overlay) = reindex_and_overlay(&files, CALLER, &edited, &["t"]);
        assert_module_tree_binds(&reindex, &[("crates/b/src/util.rs", "f")], label);
        assert_eq!(overlay, reindex, "{label}");
    }
}

/// Crate `a` names its lib root in `[lib] path`, so the overlay's target
/// roots (`lib.rs`, `main.rs`, ...) do not declare the caller and it has no
/// module chain. A crate head needs none: the edge the index binds stays.
#[test]
fn test_overlay_rust_cross_crate_custom_lib_path_caller_keeps_edge() {
    const ENTRY: &str = "crates/a/src/entry.rs";
    let caller = "pub mod util;\n\npub fn t() {\n    b::util::f();\n}\n";
    let mut files = owned(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
        ),
        (
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[lib]\npath = \"src/entry.rs\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
        ),
        ("crates/a/src/util.rs", "pub fn f() {}\n"),
        (
            "crates/b/Cargo.toml",
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
        ),
        ("crates/b/src/lib.rs", "pub mod util;\n"),
        ("crates/b/src/util.rs", "pub fn f() {}\n\npub fn g() {}\n"),
    ]);
    files.push((ENTRY.to_string(), caller.to_string()));

    let graph = index(&files);
    let entries: Vec<(&str, Vec<&str>)> = graph
        .workspace
        .scopes
        .iter()
        .map(|scope| {
            (
                scope.root.as_str(),
                scope.entry_files.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        entries,
        vec![
            ("crates/a", vec![ENTRY]),
            ("crates/b", vec!["crates/b/src/lib.rs"]),
        ],
        "the stored lib entry is the `[lib] path`"
    );

    for (label, edited) in [
        ("touched", caller.to_string()),
        ("one-line edit", one_line_edit(caller)),
    ] {
        let (reindex, overlay) = reindex_and_overlay(&files, ENTRY, &edited, &["t"]);
        assert_module_tree_binds(&reindex, &[("crates/b/src/util.rs", "f")], label);
        assert_eq!(overlay, reindex, "{label}");
    }
}

/// C2 open point 2, option (a): the index lets the last member that claims
/// a code name win, so the table stores that member alone and the overlay
/// binds what the index binds.
#[test]
fn test_graph_bin_crate_table_two_crates_same_name_last_member_wins_like_index() {
    let caller = "pub fn t() {\n    dup::x();\n}\n";
    let mut files = owned(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/x1\", \"crates/x2\"]\n",
        ),
        (
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        ),
        (
            "crates/x1/Cargo.toml",
            "[package]\nname = \"dup\"\nversion = \"0.1.0\"\n",
        ),
        ("crates/x1/src/lib.rs", "pub fn x() {}\n"),
        (
            "crates/x2/Cargo.toml",
            "[package]\nname = \"dup\"\nversion = \"0.2.0\"\n",
        ),
        ("crates/x2/src/lib.rs", "pub fn x() {}\n"),
    ]);
    files.push((CALLER.to_string(), caller.to_string()));

    let graph = index(&files);
    let bytes = archive(&graph);
    let stored = access(&bytes).workspace_facts();
    assert_eq!(
        stored, graph.workspace,
        "the archived table reads back whole"
    );
    let dup_rows: Vec<(&str, NameRank)> = stored
        .names
        .iter()
        .filter(|entry| entry.name == "dup")
        .map(|entry| (stored.scopes[entry.scope.index()].root.as_str(), entry.rank))
        .collect();
    assert_eq!(dup_rows, vec![("crates/x2", NameRank::Primary)]);

    for (label, edited) in [
        ("touched", caller.to_string()),
        ("one-line edit", one_line_edit(caller)),
    ] {
        let (reindex, overlay) = reindex_and_overlay(&files, CALLER, &edited, &["t"]);
        assert_module_tree_binds(&reindex, &[("crates/x2/src/lib.rs", "x")], label);
        assert_eq!(overlay, reindex, "{label}");
    }
}

#[test]
fn test_graph_bin_crate_table_lib_name_override_maps_head() {
    // `u` writes the package spelling, which the index accepts where no
    // crate's code name claims it.
    let caller =
        "pub mod util;\n\npub fn t() {\n    other::util::f();\n}\n\npub fn u() {\n    b_pkg::util::g();\n}\n";
    let files = two_crates(
        caller,
        "[package]\nname = \"b-pkg\"\nversion = \"0.1.0\"\n\n[lib]\nname = \"other\"\n",
    );

    let graph = index(&files);
    let names: Vec<(&str, NameRank, &str)> = graph
        .workspace
        .names
        .iter()
        .map(|entry| {
            (
                entry.name.as_str(),
                entry.rank,
                graph.workspace.scopes[entry.scope.index()].root.as_str(),
            )
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ("a", NameRank::Primary, "crates/a"),
            ("b-pkg", NameRank::Alternate, "crates/b"),
            ("b_pkg", NameRank::Alternate, "crates/b"),
            ("other", NameRank::Primary, "crates/b"),
        ]
    );

    for (label, edited) in [
        ("touched", caller.to_string()),
        ("one-line edit", one_line_edit(caller)),
    ] {
        let (reindex, overlay) = reindex_and_overlay(&files, CALLER, &edited, &["t", "u"]);
        assert_module_tree_binds(
            &reindex,
            &[("crates/b/src/util.rs", "f"), ("crates/b/src/util.rs", "g")],
            label,
        );
        assert_eq!(overlay, reindex, "{label}");
    }
}

#[test]
fn test_graph_bin_crate_table_non_rust_repo_is_empty() {
    let files = owned(&[
        ("pkg/__init__.py", ""),
        ("pkg/m.py", "def helper():\n    pass\n"),
        (
            "pkg/app.py",
            "from pkg.m import helper\n\ndef t():\n    helper()\n",
        ),
    ]);
    let graph = index(&files);
    assert!(!graph.nodes.is_empty(), "the Python files are indexed");
    assert_eq!(graph.workspace, WorkspaceFacts::default());
    let bytes = archive(&graph);
    let archived = access(&bytes);
    assert_eq!(archived.workspace.scopes.len(), 0);
    assert_eq!(archived.workspace.names.len(), 0);
    assert_eq!(archived.workspace_facts(), WorkspaceFacts::default());
}

/// Divergence D2: crate `a` writes `use b::m;` and calls `m::f()`, and also
/// `b::m::f()` and `b::m::deep::g()`, where `m` is the directory module
/// `m/mod.rs`. The overlay of the dirty caller binds each call to the
/// target, at the confidence and with the reason, the reindex gives.
#[test]
fn test_overlay_rust_cross_crate_call_from_dirty_file_matches_reindex() {
    const M: &str = "crates/b/src/m/mod.rs";
    const DEEP: &str = "crates/b/src/m/deep.rs";
    let caller = "use b::m;\n\npub fn t() {\n    b::m::f();\n}\n\npub fn u() {\n    m::f();\n}\n\npub fn v() {\n    b::m::deep::g();\n}\n";
    let mut files = owned(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
        ),
        (
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
        ),
        (
            "crates/b/Cargo.toml",
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
        ),
        ("crates/b/src/lib.rs", "pub mod m;\n"),
        (M, "pub mod deep;\n\npub fn f() {}\n"),
        (DEEP, "pub fn g() {}\n"),
    ]);
    files.push((CALLER.to_string(), caller.to_string()));

    for (label, edited) in [
        ("touched", caller.to_string()),
        ("one-line edit", one_line_edit(caller)),
    ] {
        let (reindex, overlay) = reindex_and_overlay(&files, CALLER, &edited, &["t", "u", "v"]);
        let bound: Vec<(&str, &str)> = reindex
            .iter()
            .map(|call| (call.callee_file.as_str(), call.callee.as_str()))
            .collect();
        assert_eq!(
            bound,
            vec![(M, "f"), (M, "f"), (DEEP, "g")],
            "{label}: the reindex binds each call once: {reindex:#?}"
        );
        // `b::m::f()` and `b::m::deep::g()` bind at the qualifier tier: `m`
        // and `deep` each name one file. `u` is the D2 case: its qualifier
        // `m` comes from `use b::m`, which only the crate table places.
        assert_eq!(overlay, reindex, "{label}");
    }
}

/// Goal 4: without a `Cargo.toml` the index builds no module tree, so
/// `super::helper()` gets no module-tree edge. The overlay reads the same
/// empty crate table and must not bind it through a tree of its own.
#[test]
fn test_overlay_rust_super_path_without_manifest_matches_index() {
    const CHILD: &str = "src/a.rs";
    let child = "pub fn t() {\n    super::helper();\n}\n";
    let mut files = owned(&[("src/lib.rs", "pub mod a;\n\npub fn helper() {}\n")]);
    files.push((CHILD.to_string(), child.to_string()));

    let graph = index(&files);
    assert_eq!(graph.workspace, WorkspaceFacts::default(), "no crate table");

    for (label, edited) in [
        ("touched", child.to_string()),
        ("one-line edit", one_line_edit(child)),
    ] {
        let (reindex, overlay) = reindex_and_overlay(&files, CHILD, &edited, &["t"]);
        assert!(
            reindex
                .iter()
                .all(|call| call.confidence != Tier::ModuleTree.confidence()),
            "{label}: the index has no module tree: {reindex:#?}"
        );
        assert_eq!(overlay, reindex, "{label}");
    }
}

const SHARED: &str = "src/shared.rs";

/// One package with a lib and a bin that both declare `mod shared;` (the
/// caller), each placing `mod x;` at the file named by its `#[path]`. A
/// decoy `src/x.rs` is the file layout's answer for `crate::x`.
fn lib_and_bin_sharing_a_file(lib_x: &str, main_x: &str) -> Vec<(String, String)> {
    owned(&[
        (
            "Cargo.toml",
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
        ),
        (
            "src/lib.rs",
            format!("#[path = \"{lib_x}\"]\nmod x;\nmod shared;\n").as_str(),
        ),
        (
            "src/main.rs",
            format!("#[path = \"{main_x}\"]\nmod x;\nmod shared;\nfn main() {{}}\n").as_str(),
        ),
        ("src/x.rs", "pub fn f() {}\n"),
        ("src/x_both.rs", "pub fn f() {}\n"),
        ("src/x_lib.rs", "pub fn f() {}\n"),
        ("src/x_main.rs", "pub fn f() {}\n"),
        (SHARED, "use crate::x;\n\npub fn t() {\n    x::f();\n}\n"),
    ])
}

/// A crate whose `[lib] path` has no `src/` segment: the file layout places
/// no `crate::` path there, the module tree does. `use crate::x; x::f()`
/// binds `x.rs` in the index, and the overlay of a dirty copy binds it too.
/// `other/x.rs` makes the qualifier `x` name two files, so no file-stem tier
/// stands in for the module tree.
///
/// Not covered: a `#[path]` module. The overlay's tree walks the file layout
/// (`RustModules::child`), so it cannot place one.
#[test]
fn test_overlay_rust_anchored_path_custom_lib_path_matches_index() {
    const CHILD: &str = "shared.rs";
    let child = "use crate::x;\n\npub fn t() {\n    x::f();\n}\n";
    let mut files = owned(&[
        (
            "Cargo.toml",
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lib]\npath = \"lib.rs\"\n",
        ),
        ("lib.rs", "mod x;\nmod shared;\n"),
        ("x.rs", "pub fn f() {}\n"),
        ("other/x.rs", "pub fn f() {}\n"),
    ]);
    files.push((CHILD.to_string(), child.to_string()));
    for (label, edited) in [
        ("touched", child.to_string()),
        ("one-line edit", one_line_edit(child)),
    ] {
        let (reindex, overlay) = reindex_and_overlay(&files, CHILD, &edited, &["t"]);
        assert_eq!(
            reindex
                .iter()
                .map(|call| call.callee_file.as_str())
                .collect::<Vec<_>>(),
            ["x.rs"],
            "{label}: the reindex binds the module the crate root places: {reindex:#?}"
        );
        assert_eq!(overlay, reindex, "{label}");
    }
}

/// The lib and the bin place `crate::x` at different files, so the module
/// trees name none and both sides fall back to the file layout (`src/x.rs`).
/// The overlay must not pick one target's file.
///
/// The overlay places `mod x;` by file layout alone, so it reaches `src/x.rs`
/// here whether or not it reads `#[path]`: the equality below passes either
/// way and cannot see a missing `#[path]` (FU-2026-10-26-2ad63df39f50). The
/// agree case at the end of the test can: both targets place `x` at
/// `x_both.rs`, the index binds it, and an overlay blind to `#[path]` binds
/// the decoy `src/x.rs`. That case is held against its baseline
/// (`rust_path_attr_decoy`), so this test fails when the overlay's answer
/// for it changes without the baseline.
#[test]
fn test_overlay_rust_anchored_path_targets_disagree_matches_index() {
    let files = lib_and_bin_sharing_a_file("x_lib.rs", "x_main.rs");
    let caller = files.last().expect("the caller is last").1.clone();
    for (label, edited) in [
        ("touched", caller.clone()),
        ("one-line edit", one_line_edit(&caller)),
    ] {
        let (reindex, overlay) = reindex_and_overlay(&files, SHARED, &edited, &["t"]);
        assert_eq!(
            reindex
                .iter()
                .map(|call| call.callee_file.as_str())
                .collect::<Vec<_>>(),
            ["src/x.rs"],
            "{label}: the reindex falls back to the file layout: {reindex:#?}"
        );
        assert_eq!(overlay, reindex, "{label}");
    }
    check_path_attr("rust_path_attr_decoy", true);
}

/// Rust `Self::m()` names the caller's impl type on both sides: an inherent
/// impl, a trait default method (the trait's own declaration) and a trait
/// impl reaching an inherent `new`. `U` holds a second `dup_helper` and
/// `new`, so only the caller's owner picks the edge.
#[test]
fn test_overlay_rust_self_path_call_matches_reindex() {
    const LIB: &str = "src/lib.rs";
    let lib = "pub struct S;\nimpl S {\n    pub fn new() -> S { S }\n    pub fn dup_helper() -> i32 { 1 }\n    pub fn self_dup() -> i32 { Self::dup_helper() }\n}\n\
        pub struct U;\nimpl U {\n    pub fn new() -> U { U }\n    pub fn dup_helper() -> i32 { 2 }\n}\n\
        pub trait T {\n    fn tdef_self() -> i32 { Self::thelper() }\n    fn thelper() -> i32;\n}\n\
        impl T for S {\n    fn thelper() -> i32 { Self::new(); 2 }\n}\n";
    let files = owned(&[
        (
            "Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        ),
        (LIB, lib),
    ]);
    for (label, edited, shift) in [
        ("touched", lib.to_string(), 0),
        ("one-line edit", one_line_edit(lib), 1),
    ] {
        let (reindex, overlay) =
            reindex_and_overlay(&files, LIB, &edited, &["self_dup", "tdef_self", "thelper"]);
        let bound: Vec<(&str, u32)> = reindex
            .iter()
            .map(|call| (call.callee.as_str(), call.callee_line))
            .collect();
        assert_eq!(
            bound,
            [
                ("dup_helper", 4 + shift),
                ("thelper", 14 + shift),
                ("new", 3 + shift),
            ],
            "{label}: S.dup_helper, T.thelper, S.new: {reindex:#?}"
        );
        assert_eq!(overlay, reindex, "{label}");
    }
}

/// FU-2026-10-26-045ff15cd46b: `b::m` writes `pub use self::inner::*;`
/// beside its own `f` and `h`, and `inner` declares an `h` too. The module's
/// own items shadow the glob, so both sides bind `m.rs`. A name only the
/// glob gives (`g`) stays out: the overlay cannot see whether that `use` is
/// `pub`, so it binds no edge through it.
#[test]
fn test_overlay_rust_glob_pub_use_own_item_matches_reindex() {
    const M: &str = "crates/b/src/m.rs";
    let caller = "pub fn t() {\n    b::m::f();\n    b::m::h();\n}\n";
    let mut files = owned(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
        ),
        (
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
        ),
        (
            "crates/b/Cargo.toml",
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
        ),
        ("crates/b/src/lib.rs", "pub mod m;\n"),
        (
            M,
            "mod inner;\npub use self::inner::*;\npub fn f() {}\npub fn h() {}\n",
        ),
        ("crates/b/src/m/inner.rs", "pub fn g() {}\npub fn h() {}\n"),
    ]);
    files.push((CALLER.to_string(), caller.to_string()));
    for (label, edited) in [
        ("touched", caller.to_string()),
        ("one-line edit", one_line_edit(caller)),
    ] {
        let (reindex, overlay) = reindex_and_overlay(&files, CALLER, &edited, &["t"]);
        assert_module_tree_binds(&reindex, &[(M, "f"), (M, "h")], label);
        assert_eq!(overlay, reindex, "{label}");
    }
}

/// Rust `Self::tonly()` in `impl T for S`, where `tonly` is only `T`'s
/// default method, takes the receiver-typing ladder as `S::tonly()` does,
/// on both sides. In `impl T for U` the same callee binds `U`'s inherent
/// `tonly`: two callers with different impl types never share an answer.
#[test]
fn test_overlay_rust_self_path_trait_default_matches_reindex() {
    const LIB: &str = "src/lib.rs";
    let lib = "pub trait T {\n    fn tonly() -> i32 { 1 }\n    fn s_self() -> i32 { 0 }\n    fn s_type() -> i32 { 0 }\n    fn u_self() -> i32 { 0 }\n}\n\
        pub struct S;\nimpl T for S {\n    fn s_self() -> i32 { Self::tonly() }\n    fn s_type() -> i32 { S::tonly() }\n}\n\
        pub struct U;\nimpl U {\n    pub fn tonly() -> i32 { 2 }\n}\n\
        impl T for U {\n    fn u_self() -> i32 { Self::tonly() }\n}\n";
    let files = owned(&[
        (
            "Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        ),
        (LIB, lib),
    ]);
    for (label, edited, shift) in [
        ("touched", lib.to_string(), 0),
        ("one-line edit", one_line_edit(lib), 1),
    ] {
        let (reindex, overlay) =
            reindex_and_overlay(&files, LIB, &edited, &["s_self", "s_type", "u_self"]);
        let bound: Vec<(&str, u32)> = reindex
            .iter()
            .map(|call| (call.callee.as_str(), call.callee_line))
            .collect();
        assert_eq!(
            bound,
            [
                ("tonly", 2 + shift),
                ("tonly", 2 + shift),
                ("tonly", 14 + shift)
            ],
            "{label}: T.tonly twice, U.tonly: {reindex:#?}"
        );
        assert_eq!(overlay, reindex, "{label}");
    }
}

/// `ArchivedZg` is declared only by `impl ArchivedZg` (an `rkyv::Archive`
/// derive generates the type), and `other.rs` holds a free `fn later`. A
/// typed receiver binds the impl's method at the qualifier tier, from the
/// impl's own file and through a `use`, whichever file is dirty.
#[test]
fn test_overlay_rust_typed_receiver_of_impl_only_type_matches_reindex() {
    const GRAPH: &str = "src/graph.rs";
    const USER: &str = "src/user.rs";
    let graph = "#[derive(rkyv::Archive)]\npub struct Zg;\nimpl ArchivedZg {\n    pub fn later(&self, i: u32) -> u32 { i }\n}\npub fn same_file(g: &ArchivedZg) -> u32 {\n    g.later(1)\n}\n";
    let user = "use crate::graph::ArchivedZg;\npub fn use_arch(g: &ArchivedZg) -> u32 {\n    g.later(1)\n}\n";
    let files = owned(&[
        (
            "Cargo.toml",
            "[package]\nname = \"zg\"\nversion = \"0.1.0\"\n",
        ),
        (
            "src/lib.rs",
            "pub mod graph;\npub mod other;\npub mod user;\n",
        ),
        (GRAPH, graph),
        ("src/other.rs", "pub fn later(i: u32) -> u32 { i }\n"),
        (USER, user),
    ]);
    for (caller_file, source, caller, shift) in
        [(GRAPH, graph, "same_file", 1), (USER, user, "use_arch", 0)]
    {
        for (label, edited) in [
            ("touched", source.to_string()),
            ("one-line edit", one_line_edit(source)),
        ] {
            let (reindex, overlay) = reindex_and_overlay(&files, caller_file, &edited, &[caller]);
            let line = if label == "touched" { 4 } else { 4 + shift };
            let bound: Vec<(&str, &str, u32, f32)> = reindex
                .iter()
                .map(|call| {
                    (
                        call.callee_file.as_str(),
                        call.callee.as_str(),
                        call.callee_line,
                        call.confidence,
                    )
                })
                .collect();
            assert_eq!(
                bound,
                [(GRAPH, "later", line, Tier::QualifierScoped.confidence())],
                "{caller} {label}: {reindex:#?}"
            );
            assert_eq!(overlay, reindex, "{caller} {label}");
        }
    }
}

/// R5: `impl From<Name> for String` and `impl StrExt for str` make `lib.rs`
/// the qualifier file of the impl-only `String` and `str`, beside `Name`'s
/// `len` and `trim`. On both sides a method call on a `&String` or a `&str`
/// binds no other type's method; the `str` impl's own `first_word` binds,
/// and so does `Tail`'s default `tail`, which `impl Tail for str` inherits.
#[test]
fn test_overlay_rust_typed_receiver_of_impl_only_std_type_matches_reindex() {
    const LIB: &str = "src/lib.rs";
    let lib = "pub struct Name;\nimpl Name {\n    pub fn len(&self) -> usize { 0 }\n    pub fn trim(&self) -> u32 { 0 }\n}\n\
        impl From<Name> for String {\n    fn from(_: Name) -> String { String::new() }\n}\n\
        pub trait StrExt {\n    fn first_word(&self) -> &str;\n}\n\
        impl StrExt for str {\n    fn first_word(&self) -> &str { self }\n}\n\
        pub fn n(s: &String) -> usize { s.len() }\npub fn t(s: &str) -> u32 { s.trim() }\npub fn w(s: &str) -> &str { s.first_word() }\n\
        pub trait Tail {\n    fn tail(&self) -> u8 { 0 }\n    fn other(&self);\n}\nimpl Tail for str {\n    fn other(&self) {}\n}\n\
        pub fn d(s: &str) -> u8 { s.tail() }\n";
    let files = owned(&[
        (
            "Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        ),
        (LIB, lib),
    ]);
    for (label, edited, shift) in [
        ("touched", lib.to_string(), 0),
        ("one-line edit", one_line_edit(lib), 1),
    ] {
        let (reindex, overlay) = reindex_and_overlay(&files, LIB, &edited, &["n", "t", "w", "d"]);
        let bound: Vec<(&str, u32)> = reindex
            .iter()
            .map(|call| (call.callee.as_str(), call.callee_line))
            .collect();
        assert_eq!(
            bound,
            [("first_word", 13 + shift), ("tail", 19 + shift)],
            "{label}: str's first_word and Tail's default tail: {reindex:#?}"
        );
        assert_eq!(overlay, reindex, "{label}");
    }
}

/// A `fn` inside a function body is no item of its module. Without a glob,
/// `crate::run()` and `use crate::run;` from a dirty `other.rs` bind
/// nothing on both sides, never the function-local `run`.
#[test]
fn test_overlay_rust_function_local_fn_is_no_module_item_matches_reindex() {
    const OTHER: &str = "src/other.rs";
    for user in [
        "pub fn go() {\n    crate::run();\n}\n",
        "use crate::run;\npub fn go() {\n    run();\n}\n",
    ] {
        let files = owned(&[
            (
                "Cargo.toml",
                "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/lib.rs",
                "mod other;\npub fn setup() {\n    fn run() {}\n    run();\n}\n",
            ),
            (OTHER, user),
        ]);
        for (label, edited) in [
            ("touched", user.to_string()),
            ("one-line edit", one_line_edit(user)),
        ] {
            let (reindex, overlay) = reindex_and_overlay(&files, OTHER, &edited, &["go"]);
            assert_eq!(reindex, [], "{label}: {user}");
            assert_eq!(overlay, reindex, "{label}: {user}");
        }
    }
}

/// With the root's glob `pub use inner::*`, the function-local `run` does
/// not shadow it: the reindex binds `inner.rs`. The overlay cannot see that
/// the glob `use` is `pub` (R2, FU-2026-11-02-768a5edcd559), so the contract
/// on its side is only that it never binds the function-local `run`.
#[test]
fn test_overlay_rust_function_local_fn_never_shadows_a_glob() {
    const LIB: &str = "src/lib.rs";
    let lib = "mod inner;\npub use inner::*;\npub fn setup() {\n    fn run() {}\n    run();\n}\npub fn go() {\n    crate::run();\n}\n";
    let files = owned(&[
        (
            "Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        ),
        (LIB, lib),
        ("src/inner.rs", "pub fn run() {}\n"),
    ]);
    for (label, edited) in [
        ("touched", lib.to_string()),
        ("one-line edit", one_line_edit(lib)),
    ] {
        let (reindex, overlay) = reindex_and_overlay(&files, LIB, &edited, &["go"]);
        assert_module_tree_binds(&reindex, &[("src/inner.rs", "run")], label);
        assert!(
            overlay.iter().all(|call| call.callee_file != LIB),
            "{label}: the function-local `run` must not bind: {overlay:#?}"
        );
    }
}

/// R3 on the type-path check: `m.rs` declares its own `Foo` beside a glob
/// `pub use self::inner::*` whose `inner.rs` declares a `Foo` too. In
/// Rust `crate::m::Foo` is `m.rs`'s own, so a call from `inner.rs` binds
/// `m.rs`'s `Foo::new` on both sides, never `inner.rs`'s.
#[test]
fn test_overlay_rust_type_path_own_item_shadows_glob_matches_reindex() {
    const INNER: &str = "src/m/inner.rs";
    let inner = "pub struct Foo;\nimpl Foo {\n    pub fn new() -> Foo { Foo }\n}\npub fn go() {\n    crate::m::Foo::new();\n}\n";
    let files = owned(&[
        (
            "Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        ),
        ("src/lib.rs", "pub mod m;\n"),
        (
            "src/m.rs",
            "mod inner;\npub use self::inner::*;\npub struct Foo;\nimpl Foo {\n    pub fn new() -> Foo { Foo }\n}\n",
        ),
        (INNER, inner),
    ]);
    for (label, edited) in [
        ("touched", inner.to_string()),
        ("one-line edit", one_line_edit(inner)),
    ] {
        let (reindex, overlay) = reindex_and_overlay(&files, INNER, &edited, &["go"]);
        let bound: Vec<(&str, &str, u32)> = reindex
            .iter()
            .map(|call| {
                (
                    call.callee_file.as_str(),
                    call.callee.as_str(),
                    call.callee_line,
                )
            })
            .collect();
        assert_eq!(
            bound,
            [("src/m.rs", "new", 5)],
            "{label}: m.rs's own Foo::new: {reindex:#?}"
        );
        assert_eq!(overlay, reindex, "{label}");
    }
}

/// A type declared inside `m.rs`'s `fn setup` is no item of `m`, so it
/// never shadows `m`'s glob `pub use self::inner::*`: the reindex binds
/// `inner.rs`'s `Foo::new` (the caller's own file answers first, at the
/// qualifier tier; before the fix the body-local one bound at 1.0 through
/// the module tree). The overlay cannot see
/// that the glob is `pub` (R2, FU-2026-11-02-768a5edcd559), so on its side
/// the contract is that it never binds the body-local `Foo::new` of the
/// clean `m.rs`, which it did before the fix.
#[test]
fn test_overlay_rust_body_local_type_never_shadows_a_glob() {
    const INNER: &str = "src/m/inner.rs";
    let inner = "pub struct Foo;\nimpl Foo {\n    pub fn new() -> Foo { Foo }\n}\npub fn go() {\n    crate::m::Foo::new();\n}\n";
    let files = owned(&[
        (
            "Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        ),
        ("src/lib.rs", "pub mod m;\n"),
        (
            "src/m.rs",
            "pub mod inner;\npub use self::inner::*;\nfn setup() {\n    struct Foo;\n    impl Foo {\n        fn new() -> Foo { Foo }\n    }\n}\n",
        ),
        (INNER, inner),
    ]);
    for (label, edited) in [
        ("touched", inner.to_string()),
        ("one-line edit", one_line_edit(inner)),
    ] {
        let (reindex, overlay) = reindex_and_overlay(&files, INNER, &edited, &["go"]);
        let bound: Vec<(&str, &str)> = reindex
            .iter()
            .map(|call| (call.callee_file.as_str(), call.callee.as_str()))
            .collect();
        assert_eq!(bound, [(INNER, "new")], "{label}: {reindex:#?}");
        assert!(
            overlay.iter().all(|call| call.callee_file != "src/m.rs"),
            "{label}: the body-local `Foo::new` must not bind: {overlay:#?}"
        );
    }
}

/// R5 on both sides: `self.len()` in `impl Summary for String` and
/// `s.len()` on a `&String` beside `impl From<Name> for String` call std.
/// Neither binds `Name::len` nor `Sizer`'s default `len`: `String` does not
/// implement `Sizer`. The second fixture calls from `user.rs`, so the
/// overlay reads `lib.rs`'s impl blocks from the archived graph.
#[test]
fn test_overlay_rust_impl_only_std_type_never_binds_a_foreign_method_matches_reindex() {
    let types = "pub struct Name;\nimpl Name {\n    pub fn len(&self) -> usize { 0 }\n}\n\
        pub trait Summary {\n    fn summarize(&self) -> usize;\n}\n\
        impl Summary for String {\n    fn summarize(&self) -> usize { self.len() }\n}\n";
    let sizer = "pub struct Name;\npub trait Sizer {\n    fn len(&self) -> usize { 0 }\n    fn z(&self);\n}\n\
        impl Sizer for Name {\n    fn z(&self) {}\n}\n\
        impl From<Name> for String {\n    fn from(_: Name) -> String { String::new() }\n}\n";
    let manifest = (
        "Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
    );
    let sizer_lib = format!("{sizer}pub fn n(s: &String) -> usize {{ s.len() }}\n");
    let sizer_root = format!("mod user;\n{sizer}");
    let user = "pub fn n(s: &String) -> usize { s.len() }\n";
    for (files, dirty, callers) in [
        (
            owned(&[manifest, ("src/lib.rs", types)]),
            "src/lib.rs",
            &["summarize"][..],
        ),
        (
            owned(&[manifest, ("src/lib.rs", sizer_lib.as_str())]),
            "src/lib.rs",
            &["n"][..],
        ),
        (
            owned(&[
                manifest,
                ("src/lib.rs", sizer_root.as_str()),
                ("src/user.rs", user),
            ]),
            "src/user.rs",
            &["n"][..],
        ),
    ] {
        let source = files
            .iter()
            .find(|(path, _)| path == dirty)
            .map(|(_, source)| source.clone())
            .expect("the dirty file is a fixture file");
        for (label, edited) in [
            ("touched", source.clone()),
            ("one-line edit", one_line_edit(&source)),
        ] {
            let (reindex, overlay) = reindex_and_overlay(&files, dirty, &edited, callers);
            assert!(
                reindex.iter().all(|call| call.callee != "len"),
                "{label} {dirty}: std's len: {reindex:#?}"
            );
            assert_eq!(overlay, reindex, "{label} {dirty}");
        }
    }
}

/// `crate::helper()` reaches `m.rs` through the root's `pub use m::helper;`,
/// where `impl S { pub fn helper(&self) }` comes before the free `helper`.
/// The reindex binds the free `fn` through the module tree. The overlay
/// cannot see that the `use` is `pub` (R2), so its contract is that it
/// never binds the method.
#[test]
fn test_overlay_rust_module_path_never_binds_a_method_namesake() {
    const OTHER: &str = "src/other.rs";
    let other = "pub fn go() {\n    crate::helper();\n}\n";
    let files = owned(&[
        (
            "Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        ),
        ("src/lib.rs", "pub mod m;\nmod other;\npub use m::helper;\n"),
        (
            "src/m.rs",
            "pub struct S;\nimpl S {\n    pub fn helper(&self) {}\n}\npub fn helper() {}\n",
        ),
        (OTHER, other),
    ]);
    for (label, edited) in [
        ("touched", other.to_string()),
        ("one-line edit", one_line_edit(other)),
    ] {
        let (reindex, overlay) = reindex_and_overlay(&files, OTHER, &edited, &["go"]);
        assert_module_tree_binds(&reindex, &[("src/m.rs", "helper")], label);
        assert!(
            reindex
                .iter()
                .chain(&overlay)
                .all(|call| call.callee_line != 3),
            "{label}: the method `S::helper` must not bind: {reindex:#?} {overlay:#?}"
        );
    }
}

/// `m.rs` declares `Foo` inside `fn setup` and again at module level. Rust
/// scopes only `fn` by its parent, so both share a uid and the body-local
/// one survives Pass 1 with the module-level one as its twin; the merged
/// node is a module item. `use crate::m::Foo; Foo::new()` and the module
/// path `crate::m::Foo::new()` bind `m.rs`'s module-level `new` on both
/// sides, with `lib.rs` dirty (the overlay reads `m.rs`'s survivor and its
/// tombstoned twin from the archive) and with `m.rs` dirty (the twin is a
/// fresh symbol). With a body-local `impl Foo { fn new }` the module-level
/// `new` is a later overload of the body-local one. `other.rs` declares a
/// second `Foo`, so no tier reaches `m.rs` by the name alone. Before the
/// fix the reindex bound nothing through the import and the overlay with
/// `m.rs` dirty answered from the twin.
#[test]
fn test_overlay_rust_module_level_twin_of_body_local_type_binds_matches_reindex() {
    const LIB: &str = "src/lib.rs";
    const M: &str = "src/m.rs";
    let by_import =
        "pub mod m;\nmod other;\nuse crate::m::Foo;\npub fn go() {\n    Foo::new();\n}\n";
    let by_path = "pub mod m;\nmod other;\npub fn go() {\n    crate::m::Foo::new();\n}\n";
    let twin = "fn setup() {\n    struct Foo;\n}\npub struct Foo;\nimpl Foo {\n    pub fn new() -> Foo { Foo }\n}\n";
    let twin_impl = "fn setup() {\n    struct Foo;\n    impl Foo {\n        fn new() -> Foo { Foo }\n    }\n}\npub struct Foo;\nimpl Foo {\n    pub fn new() -> Foo { Foo }\n}\n";
    let other = "pub struct Foo;\nimpl Foo {\n    pub fn new() -> Foo { Foo }\n}\n";
    let new_line = |m: &str| {
        m.lines()
            .position(|line| line.starts_with("    pub fn new"))
            .expect("a module-level `new`") as u32
            + 1
    };
    for (lib, m) in [(by_import, twin), (by_path, twin), (by_path, twin_impl)] {
        let files = owned(&[
            (
                "Cargo.toml",
                "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
            ),
            (LIB, lib),
            (M, m),
            ("src/other.rs", other),
        ]);
        for (dirty, source) in [(LIB, lib), (M, m)] {
            for (label, edited) in [
                ("touched", source.to_string()),
                ("one-line edit", one_line_edit(source)),
            ] {
                let (reindex, overlay) =
                    reindex_and_overlay_of(&files, dirty, &edited, LIB, &["go"]);
                let m_now = if dirty == M { edited.as_str() } else { m };
                let bound: Vec<(&str, &str, u32)> = reindex
                    .iter()
                    .map(|call| {
                        (
                            call.callee_file.as_str(),
                            call.callee.as_str(),
                            call.callee_line,
                        )
                    })
                    .collect();
                assert_eq!(
                    bound,
                    [(M, "new", new_line(m_now))],
                    "{label} {dirty} {lib}: {reindex:#?}"
                );
                assert_eq!(overlay, reindex, "{label} {dirty} {lib}");
            }
        }
    }
}

/// R5's trait arm through a blanket `impl<T: Debug> Describe for T` on both
/// sides: `self.describe()` in `impl Summary for String` and `s.describe()`
/// on a `&String` beside `impl From<X> for String` bind `Describe`'s
/// default `describe`. The third fixture calls from `user.rs`, so the
/// overlay reads `lib.rs`'s impl blocks from the archived graph. Before the
/// fix neither side bound it.
#[test]
fn test_overlay_rust_trait_default_through_blanket_impl_matches_reindex() {
    let describe = "pub trait Describe {\n    fn describe(&self) -> String { String::new() }\n}\n\
        impl<T: std::fmt::Debug> Describe for T {}\n";
    let by_self = format!(
        "{describe}pub trait Summary {{\n    fn summarize(&self) -> String;\n}}\n\
        impl Summary for String {{\n    fn summarize(&self) -> String {{ self.describe() }}\n}}\n"
    );
    let from_x = format!(
        "{describe}pub struct X;\nimpl From<X> for String {{\n    fn from(_: X) -> String {{ String::new() }}\n}}\n"
    );
    let by_typed = format!("{from_x}pub fn n(s: &String) -> String {{ s.describe() }}\n");
    let root = format!("mod user;\n{from_x}");
    let user = "pub fn n(s: &String) -> String {\n    s.describe()\n}\n";
    let manifest = (
        "Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
    );
    for (files, dirty, caller) in [
        (
            owned(&[manifest, ("src/lib.rs", by_self.as_str())]),
            "src/lib.rs",
            "summarize",
        ),
        (
            owned(&[manifest, ("src/lib.rs", by_typed.as_str())]),
            "src/lib.rs",
            "n",
        ),
        (
            owned(&[
                manifest,
                ("src/lib.rs", root.as_str()),
                ("src/user.rs", user),
            ]),
            "src/user.rs",
            "n",
        ),
    ] {
        let source = files
            .iter()
            .find(|(path, _)| path == dirty)
            .map(|(_, source)| source.clone())
            .expect("the dirty file is a fixture file");
        for (label, edited) in [
            ("touched", source.clone()),
            ("one-line edit", one_line_edit(&source)),
        ] {
            let (reindex, overlay) = reindex_and_overlay(&files, dirty, &edited, &[caller]);
            let bound: Vec<(&str, &str)> = reindex
                .iter()
                .filter(|call| call.callee == "describe")
                .map(|call| (call.callee_file.as_str(), call.callee.as_str()))
                .collect();
            assert_eq!(
                bound,
                [("src/lib.rs", "describe")],
                "{label} {dirty}: {reindex:#?}"
            );
            assert_eq!(overlay, reindex, "{label} {dirty}");
        }
    }
}

/// `m.rs` declares `mod inner { pub fn helper }` inside `fn setup` and again
/// at module level. A `mod` breaks the lexical link, so both `helper`s are
/// unscoped `Function`s owned by `inner`: they share a uid (a `Function`
/// keeps no overloads), and Pass 1 keeps the body-local one with the
/// module-level one as its tombstoned twin. The merged node is a module
/// item, so `crate::m::inner::helper()` binds it through the module tree on
/// both sides, with `lib.rs` dirty (the overlay reads `m.rs` from the
/// archive) and with `m.rs` dirty. `other.rs` declares a second
/// `inner::helper`, so no tier reaches `m.rs` by the name alone. Before the
/// fix the overlay with `m.rs` clean took every callable inside a body for
/// body-local and bound nothing.
#[test]
fn test_overlay_rust_module_level_twin_of_body_local_fn_binds_matches_reindex() {
    const LIB: &str = "src/lib.rs";
    const M: &str = "src/m.rs";
    let lib = "pub mod m;\nmod other;\npub fn go() {\n    crate::m::inner::helper();\n}\n";
    let m = "fn setup() {\n    mod inner {\n        pub fn helper() {}\n    }\n}\n\
        pub mod inner {\n    pub fn helper() {}\n}\n";
    let other = "pub mod inner {\n    pub fn helper() {}\n}\n";
    let files = owned(&[
        (
            "Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        ),
        (LIB, lib),
        (M, m),
        ("src/other.rs", other),
    ]);
    for (dirty, source) in [(LIB, lib), (M, m)] {
        for (label, edited) in [
            ("touched", source.to_string()),
            ("one-line edit", one_line_edit(source)),
        ] {
            let (reindex, overlay) = reindex_and_overlay_of(&files, dirty, &edited, LIB, &["go"]);
            assert_module_tree_binds(&reindex, &[(M, "helper")], &format!("{label} {dirty}"));
            assert_eq!(overlay, reindex, "{label} {dirty}");
        }
    }
}

/// `lib_and_bin_sharing_a_file("x_both.rs", "x_both.rs")` through the
/// strict-parity harness, with `src/x.rs` kept as the decoy or removed. The
/// dirty file is the caller `src/shared.rs`, edited by one comment line.
///
/// With the decoy (the `rust_path_attr_decoy` baseline, checked inside
/// `test_overlay_rust_anchored_path_targets_disagree_matches_index`): lib
/// and bin both place `mod x;` at `x_both.rs`, the index binds it, and the
/// overlay binds the decoy at the same confidence, a fabricated edge.
/// Expected: `edge full-only x1 Calls Function src/shared.rs:4 t -> Function
/// src/x_both.rs:1 f reason=call conf=0.85` and `edge overlay-only x1 Calls`
/// the same caller to `Function src/x.rs:1 f`, `reason=call conf=0.85` (the
/// `l1-overlay` marker reads as `call`). FU-2026-10-26-2ad63df39f50.
fn check_path_attr(name: &str, decoy: bool) {
    let files: Vec<(String, String)> = lib_and_bin_sharing_a_file("x_both.rs", "x_both.rs")
        .into_iter()
        .filter(|(path, _)| decoy || path != "src/x.rs")
        .collect();
    let caller = files
        .iter()
        .find(|(path, _)| path == SHARED)
        .expect("the caller is a fixture file")
        .1
        .clone();
    let edited = one_line_edit(&caller);
    let tree: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect();
    check(&Scenario {
        name,
        owner: "FU-2026-10-26-2ad63df39f50 (P3: the overlay places #[path] modules)",
        files: &tree,
        edits: &[Edit::Write(SHARED, &edited)],
    });
}

/// The same tree without the decoy: the index binds `x_both.rs`, the overlay
/// places `mod x;` at the missing `src/x.rs` and binds nothing.
///
/// Expected in the baseline, one line: `edge full-only x1 Calls Function
/// src/shared.rs:4 t -> Function src/x_both.rs:1 f reason=call conf=0.85`.
/// FU-2026-10-26-2ad63df39f50.
#[test]
fn test_check_rust_path_attr_agreeing_targets_no_decoy_matches_baseline() {
    check_path_attr("rust_path_attr_no_decoy", false);
}

/// Crate `a` calls `b::f()`, and crate `b` declares `f` in `util.rs` and
/// re-exports it with `pub use util::f;`. Crate `a` has its own `util::f`, so
/// only the crate head tells the two apart. The index follows the re-export;
/// the overlay follows it when the view reads `b`'s root text.
///
/// Expected in the baseline: empty (a guard: both sides bind `util.rs`).
/// FU-2026-10-08-3137f86a15e9 and FU-2026-10-25-c68c18feaf6f.
#[test]
fn test_check_rust_crate_head_through_pub_use_matches_baseline() {
    let caller = "mod util;\n\npub fn t() {\n    b::f();\n}\n";
    let edited = one_line_edit(caller);
    check(&Scenario {
        name: "rust_crate_head_pub_use",
        owner: "FU-2026-10-08-3137f86a15e9 (P3: crate head follows pub use)",
        files: &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
            ),
            (
                "crates/a/Cargo.toml",
                "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
            ),
            ("crates/a/src/util.rs", "pub fn f() {}\n"),
            (
                "crates/b/Cargo.toml",
                "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
            ),
            ("crates/b/src/lib.rs", "mod util;\npub use util::f;\n"),
            ("crates/b/src/util.rs", "pub fn f() {}\n\npub fn g() {}\n"),
            (CALLER, caller),
        ],
        edits: &[Edit::Write(CALLER, &edited)],
    });
}

/// Crate `a` calls `b::f()`, and crate `b`'s root re-exports `inner.rs`'s
/// `f` with `#[doc(inline)] pub use inner::f;`. The index's `pub_use_re`
/// matches a `pub use` only at the start of its line, so it records no row
/// for that use; the overlay reads the module's rows with the same rule
/// (`ecp_core::analyzer::rust_pub_use`) when the view reads its text.
///
/// Expected in the baseline: empty (a guard: both sides miss the use, so
/// both bind nothing through it). FU-2026-11-02-c9367f71b19b tracks the
/// index rule, which a fix changes on both sides at once.
#[test]
fn test_check_rust_pub_use_attr_same_line_matches_baseline() {
    let caller = "pub fn t() {\n    b::f();\n}\n";
    let edited = one_line_edit(caller);
    check(&Scenario {
        name: "rust_pub_use_attr_same_line",
        owner: "index pub_use_re misses an attribute-prefixed pub use (FU-2026-11-02-c9367f71b19b)",
        files: &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
            ),
            (
                "crates/a/Cargo.toml",
                "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
            ),
            (
                "crates/b/Cargo.toml",
                "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
            ),
            (
                "crates/b/src/lib.rs",
                "mod inner;\n#[doc(inline)] pub use inner::f;\n",
            ),
            ("crates/b/src/inner.rs", "pub fn f() {}\n"),
            (CALLER, caller),
        ],
        edits: &[Edit::Write(CALLER, &edited)],
    });
}

/// Guard, not a repro: a crate rooted at `[lib] path = "src/entry.rs"`
/// whose dirty entry file calls `self::x::f()` and `crate::x::f()`, and
/// whose dirty `src/m/n.rs` calls `crate::top()` and `super::helper()`.
/// Same-named items in `other/` and `other2/` keep a name tier from standing
/// in for the module tree. The index and the overlay agree on all four
/// calls on the installed 0.17.2, so the baseline stays empty;
/// FU-2026-10-25-c68c18feaf6f lists this shape as a gap, and a change of
/// `rust_module_dir` that breaks it fails here.
#[test]
fn test_check_rust_custom_lib_path_chain_matches_empty_baseline() {
    let entry = "mod x;\nmod m;\n\npub fn top() {}\n\npub fn t() {\n}\n";
    let entry_edited = "mod x;\nmod m;\n\npub fn top() {}\n\npub fn t() {\n    self::x::f();\n    crate::x::f();\n}\n";
    let nested = "pub fn t() {\n}\n";
    let nested_edited = "pub fn t() {\n    crate::top();\n    super::helper();\n}\n";
    check(&Scenario {
        name: "rust_custom_lib_path_chain",
        owner:
            "guard for FU-2026-10-25-c68c18feaf6f (custom [lib] path): this baseline stays empty",
        files: &[
            (
                "Cargo.toml",
                "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lib]\npath = \"src/entry.rs\"\n",
            ),
            ("src/entry.rs", entry),
            ("src/x.rs", "pub fn f() {}\n"),
            ("src/m.rs", "pub fn helper() {}\nmod n;\n"),
            ("src/m/n.rs", nested),
            ("other/x.rs", "pub fn f() {}\n"),
            ("other2/x.rs", "pub fn f() {}\n"),
            ("other/d.rs", "pub fn top() {}\npub fn helper() {}\n"),
            ("other2/d.rs", "pub fn top() {}\npub fn helper() {}\n"),
        ],
        edits: &[
            Edit::Write("src/entry.rs", entry_edited),
            Edit::Write("src/m/n.rs", nested_edited),
        ],
    });
}

/// Workspace `crates/a` and `crates/b`, where `b`'s root holds `b_lib` and
/// declares `mod make;` (`build`, `other`); `crates/a/src/lib.rs` is
/// `caller_source`, and `extra` adds files of `b`.
fn reexport_crates(
    caller_source: &str,
    b_lib: &str,
    extra: &[(&str, &str)],
) -> Vec<(String, String)> {
    let mut files = owned(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
        ),
        (
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
        ),
        (
            "crates/b/Cargo.toml",
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
        ),
        ("crates/b/src/lib.rs", b_lib),
        (
            "crates/b/src/make.rs",
            "pub fn build() {}\n\npub fn other() {}\n",
        ),
    ]);
    files.extend(owned(extra));
    files.push((CALLER.to_string(), caller_source.to_string()));
    files
}

/// `(callee file, callee, confidence, reason)` of each call: the contract
/// these tests read, without the callee's line.
fn targets(calls: &[Call]) -> Vec<(&str, &str, f32, &str)> {
    calls
        .iter()
        .map(|call| {
            (
                call.callee_file.as_str(),
                call.callee.as_str(),
                call.confidence,
                call.reason.as_str(),
            )
        })
        .collect()
}

/// `use b::build;` binds through `b`'s `pub use make::build;` with only the
/// importer dirty: `b`'s root and `make.rs` are clean, read from the
/// archive, whose import rows keep no visibility. The index's module-tree
/// import binds `make.rs`'s `build` at the import tier; the overlay must
/// too, not fall to Tier 3's unique `build` (0.7).
#[test]
fn test_overlay_view_dirty_importer_clean_pub_use_reexport_binds_like_index() {
    let caller = "use b::build;\n\npub fn t() {\n    build();\n}\n";
    let files = reexport_crates(caller, "mod make;\n\npub use make::build;\n", &[]);
    let (reindex, overlay) = reindex_and_overlay(&files, CALLER, &one_line_edit(caller), &["t"]);
    assert_eq!(
        targets(&reindex),
        [(
            "crates/b/src/make.rs",
            "build",
            Tier::ImportScoped.confidence(),
            "call"
        )],
        "the index binds through the pub use: {reindex:#?}"
    );
    assert_eq!(overlay, reindex);
}

/// Guard: `b`'s root has a `use` (of `build`), but the importer's path
/// names `other`, which `b`'s root neither declares nor imports. No `use`
/// stands on that path, so neither side binds it through the module tree.
#[test]
fn test_overlay_view_path_through_module_without_its_use_binds_no_tree_edge() {
    let caller = "use b::other;\n\npub fn t() {\n    other();\n}\n";
    let files = reexport_crates(caller, "mod make;\n\npub use make::build;\n", &[]);
    let (reindex, overlay) = reindex_and_overlay(&files, CALLER, &one_line_edit(caller), &["t"]);
    assert!(
        reindex
            .iter()
            .all(|call| call.confidence < Tier::ImportScoped.confidence()),
        "the index binds `other` through no tree: {reindex:#?}"
    );
    assert_eq!(overlay, reindex);
}

/// Guard: a descendant of `b`'s root sees the root's private
/// `use make::build;`, which the index does not follow as a re-export. The
/// overlay cannot tell it from a `pub use` there, so it must not follow it
/// either.
#[test]
fn test_overlay_view_descendant_importer_private_use_binds_like_index() {
    const SUB: &str = "crates/b/src/sub.rs";
    let sub = "use crate::build;\n\npub fn s() {\n    build();\n}\n";
    let files = reexport_crates(
        "pub fn t() {}\n",
        "mod make;\nmod sub;\n\nuse make::build;\n",
        &[(SUB, sub)],
    );
    let (reindex, overlay) = reindex_and_overlay(&files, SUB, &one_line_edit(sub), &["s"]);
    assert_eq!(overlay, reindex);
}

/// `b`'s root writes `pub use crate::actual::*;` beside `b_use`, a `use
/// crate::wrong::f;` that rustc drops, and `actual` and `wrong` both declare
/// `f`. The index follows the glob `pub use` to `actual.rs`'s `f`. The
/// import rows keep no visibility, so the overlay reads the root's `pub use`
/// rows from its text and binds as the index does: `actual.rs`'s `f`, never
/// `wrong.rs`'s.
fn check_named_use_beside_pub_glob(b_use: &str) {
    const ACTUAL: &str = "crates/b/src/actual.rs";
    const WRONG: &str = "crates/b/src/wrong.rs";
    let caller = "use b::f;\n\npub fn t() {\n    f();\n}\n";
    let b_lib = format!("mod actual;\nmod wrong;\n\npub use crate::actual::*;\n\n{b_use}");
    let files = reexport_crates(
        caller,
        &b_lib,
        &[(ACTUAL, "pub fn f() {}\n"), (WRONG, "pub fn f() {}\n")],
    );
    let (reindex, overlay) = reindex_and_overlay(&files, CALLER, &one_line_edit(caller), &["t"]);
    let bound: Vec<(&str, &str)> = reindex
        .iter()
        .map(|call| (call.callee_file.as_str(), call.callee.as_str()))
        .collect();
    assert_eq!(
        bound,
        [(ACTUAL, "f")],
        "the index binds through the glob pub use: {reindex:#?}"
    );
    assert_eq!(overlay, reindex);
}

#[test]
fn test_overlay_view_named_use_beside_pub_glob_binds_nothing_beyond_index() {
    check_named_use_beside_pub_glob("#[cfg(any())]\nuse crate::wrong::f;\n");
}

#[test]
fn test_overlay_view_nested_test_use_beside_pub_glob_binds_nothing_beyond_index() {
    check_named_use_beside_pub_glob(
        "#[cfg(test)]\nmod tests {\n    use crate::wrong::f;\n\n    #[test]\n    fn t() {\n        f();\n    }\n}\n",
    );
}

/// `b`'s root holds a private `use crate::inner::*;` and a `macro_rules!`
/// invocation that makes `f`, which no parse sees. The index records no
/// `pub use` there, so it binds `f` through no tree; the overlay reads the
/// same rows from the root's text, so it must not follow the glob either.
/// `other.rs` declares a second `f`, which keeps Tier 3 from binding.
#[test]
fn test_overlay_view_private_glob_macro_item_binds_like_index() {
    let caller = "use b::f;\n\npub fn t() {\n    f();\n}\n";
    let b_lib = "mod inner;\nmod other;\n\nuse crate::inner::*;\n\nmacro_rules! mk {\n    ($n:ident) => {\n        pub fn $n() {}\n    };\n}\n\nmk!(f);\n";
    let files = reexport_crates(
        caller,
        b_lib,
        &[
            ("crates/b/src/inner.rs", "pub fn f() {}\n"),
            ("crates/b/src/other.rs", "pub fn f() {}\n"),
        ],
    );
    let (reindex, overlay) = reindex_and_overlay(&files, CALLER, &one_line_edit(caller), &["t"]);
    assert!(
        reindex
            .iter()
            .all(|call| call.confidence < Tier::ImportScoped.confidence()),
        "the index binds `f` through no tree: {reindex:#?}"
    );
    assert_eq!(overlay, reindex);
}

/// `b`'s root publishes `unix.rs`'s `f` through a `pub use` inside a macro
/// invocation (the `cfg_if!` shape), which no parse sees as an import, and a
/// `#[cfg(test)]` module holds a private `use crate::wrong::f;`. The index
/// reads the `pub use` line and binds `unix.rs`. The overlay reads the same
/// line from the module's text, so it never follows the private `use` to
/// `wrong.rs`. It binds nothing yet: the `mod unix;` inside the macro is
/// no module its layout guess can place, a gap overlay P3's module-tree
/// rows close.
#[test]
fn test_overlay_view_named_use_beside_macro_published_item_binds_nothing_beyond_index() {
    const UNIX: &str = "crates/b/src/unix.rs";
    let caller = "use b::f;\n\npub fn t() {\n    f();\n}\n";
    let b_lib = "mod wrong;\nmod other;\n\nmacro_rules! items {\n    ($($i:item)*) => { $($i)* };\n}\n\nitems! {\n    mod unix;\n    pub use self::unix::f;\n}\n\n#[cfg(test)]\nmod tests {\n    use crate::wrong::f;\n}\n";
    let files = reexport_crates(
        caller,
        b_lib,
        &[
            (UNIX, "pub fn f() {}\n"),
            ("crates/b/src/wrong.rs", "pub fn f() {}\n"),
            ("crates/b/src/other.rs", "pub fn f() {}\n"),
        ],
    );
    let (reindex, overlay) = reindex_and_overlay(&files, CALLER, &one_line_edit(caller), &["t"]);
    let bound: Vec<(&str, &str)> = reindex
        .iter()
        .map(|call| (call.callee_file.as_str(), call.callee.as_str()))
        .collect();
    assert_eq!(
        bound,
        [(UNIX, "f")],
        "the index binds through the macro's pub use: {reindex:#?}"
    );
    assert!(
        overlay.iter().all(|call| reindex.contains(call)),
        "the overlay binds an edge the index does not: {overlay:#?}"
    );
}

/// Crate `b` whose root is `b_lib`, `inner.rs` declaring `helper`, and an
/// undeclared `decoy.rs` declaring a second `helper`, which keeps Tier 3
/// from binding; the dirty caller in `a` calls `helper()` through
/// `use b::helper;`. The index binds `inner.rs`'s `helper` through `b`'s
/// `pub use` row, and the overlay must bind it the same way.
fn check_pub_use_reaches_inner_helper(b_lib: &str) {
    const INNER: &str = "crates/b/src/inner.rs";
    const DECOY: &str = "crates/b/src/decoy.rs";
    let caller = "use b::helper;\n\npub fn t() {\n    helper();\n}\n";
    let files = reexport_crates(
        caller,
        b_lib,
        &[
            (INNER, "pub fn helper() {}\n"),
            (DECOY, "pub fn helper() {}\n"),
        ],
    );
    let (reindex, overlay) = reindex_and_overlay(&files, CALLER, &one_line_edit(caller), &["t"]);
    let bound: Vec<(&str, &str)> = reindex
        .iter()
        .map(|call| (call.callee_file.as_str(), call.callee.as_str()))
        .collect();
    assert_eq!(
        bound,
        [(INNER, "helper")],
        "the index binds through b's pub use: {reindex:#?}"
    );
    assert_eq!(overlay, reindex);
}

/// `b`'s root holds `pub use inner::helper;` and a `#[cfg(test)] mod tests
/// { use super::*; }`: the import rows show a glob `use` beside the named
/// one, with no visibility. The root's text holds one `pub use` row, the
/// named one, which both sides follow.
#[test]
fn test_overlay_view_pub_use_beside_test_mod_glob_binds_like_index() {
    check_pub_use_reaches_inner_helper(
        "mod inner;\npub use inner::helper;\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n}\n",
    );
}

/// `b`'s root is a facade, `pub use inner::*;`: both sides follow the glob
/// row to `inner.rs`'s `helper`.
#[test]
fn test_overlay_view_pub_glob_facade_binds_like_index() {
    check_pub_use_reaches_inner_helper("mod inner;\npub use inner::*;\n");
}

/// `b`'s root declares `#[path = "actual.rs"] pub mod facade;`, and a stray
/// `facade.rs` no `mod` declares sits where the file layout puts `facade`.
/// `actual.rs` re-exports `right`'s `f`, the stray file `wrong`'s. The index
/// places `facade` at `actual.rs` and binds `right.rs`. The overlay's layout
/// places `facade` at the stray file; reading that file's `pub use` would
/// bind `wrong.rs`. The overlay must bind nothing the index does not. Without
/// the stray file the test would hit only the known missing edge of the
/// `#[path]` layout gap (FU-2026-10-26-2ad63df39f50).
#[test]
fn test_overlay_view_path_attr_beside_stray_conventional_file_binds_nothing_beyond_index() {
    const RIGHT: &str = "crates/b/src/right.rs";
    const WRONG: &str = "crates/b/src/wrong.rs";
    let caller = "use b::facade::f;\n\npub fn t() {\n    f();\n}\n";
    let files = reexport_crates(
        caller,
        "#[path = \"actual.rs\"]\npub mod facade;\nmod right;\nmod wrong;\n",
        &[
            ("crates/b/src/actual.rs", "pub use crate::right::*;\n"),
            ("crates/b/src/facade.rs", "pub use crate::wrong::*;\n"),
            (RIGHT, "pub fn f() {}\n"),
            (WRONG, "pub fn f() {}\n"),
        ],
    );
    let (reindex, overlay) = reindex_and_overlay(&files, CALLER, &one_line_edit(caller), &["t"]);
    let bound: Vec<(&str, &str)> = reindex
        .iter()
        .map(|call| (call.callee_file.as_str(), call.callee.as_str()))
        .collect();
    assert_eq!(
        bound,
        [(RIGHT, "f")],
        "the index binds through the #[path] module: {reindex:#?}"
    );
    assert!(
        overlay.iter().all(|call| call.callee_file != WRONG),
        "the overlay follows the stray file's pub use: {overlay:#?}"
    );
    assert!(
        overlay.iter().all(|call| reindex.contains(call)),
        "the overlay binds an edge the index does not: {overlay:#?}"
    );
}

/// `b`'s root declares `#[path = "right.rs"] pub mod facade;` beside
/// `pub mod right;`, so one file serves two `mod`s. The index walks the tree
/// once per file and places `right.rs` as `facade`. The overlay's layout
/// places it as `right`; no step of `right`'s path carries `#[path]`, yet
/// following its glob there would bind an edge the index does not. A
/// `#[path]` anywhere on the parents keeps the overlay out.
#[test]
fn test_overlay_view_path_attr_onto_sibling_file_binds_nothing_beyond_index() {
    let caller = "use b::right::f;\n\npub fn t() {\n    f();\n}\n";
    let files = reexport_crates(
        caller,
        "#[path = \"right.rs\"]\npub mod facade;\npub mod right;\nmod x;\nmod y;\n",
        &[
            ("crates/b/src/right.rs", "pub use crate::x::*;\n"),
            ("crates/b/src/x.rs", "pub fn f() {}\n"),
            ("crates/b/src/y.rs", "pub fn f() {}\n"),
        ],
    );
    let (reindex, overlay) = reindex_and_overlay(&files, CALLER, &one_line_edit(caller), &["t"]);
    assert!(
        overlay.iter().all(|call| reindex.contains(call)),
        "the overlay binds an edge the index does not: {overlay:#?}\nindex: {reindex:#?}"
    );
}
