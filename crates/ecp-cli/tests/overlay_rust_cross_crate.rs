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
//! What the fixtures do not cover: `pub use` re-export chains behind a crate
//! head (`graph.bin` keeps no import visibility, so the overlay binds no
//! edge through a `use` the index follows), inline `mod util { .. }`
//! modules, and `#[path]` module files. The overlay walks file-backed `mod`
//! declarations only.

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
/// the edit over the index of `files`.
fn reindex_and_overlay(
    files: &[(String, String)],
    caller_file: &str,
    edited: &str,
    callers: &[&str],
) -> (Vec<Call>, Vec<Call>) {
    let edited_tree: Vec<(String, String)> = files
        .iter()
        .map(|(path, source)| {
            let source = if path == caller_file { edited } else { source };
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
    let inputs: Vec<OverlayFileInput> = vec![parse(caller_file, edited).into()];
    let view = OverlayView::build(base, &inputs).expect("an edit makes a view");
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
