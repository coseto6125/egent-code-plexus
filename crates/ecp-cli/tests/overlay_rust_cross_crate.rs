//! The Rust crate table in `graph.bin` (contract C2, Cargo rows) and the
//! overlay's `<crate_name>::path::f()` heads that read it.
//!
//! A full index binds a call into another workspace crate through its
//! module tree (`RustWorkspaceModTree::resolve_fqn`, Tier 3.5). The overlay
//! has no file system walk: it reads the crate name → entry file rows the
//! index stored, then walks the stored `mod` declarations. Each test indexes
//! a workspace from a temporary root that holds every file, as `ecp admin
//! index` does, then builds the overlay of a dirty caller over that index.
//!
//! What the fixtures do not cover: `pub use` re-export chains behind a crate
//! head, inline `mod util { .. }` modules, and a caller file that no target
//! root declares. The overlay walks file-backed `mod` declarations only.

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
