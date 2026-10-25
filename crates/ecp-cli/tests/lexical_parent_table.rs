//! The `graph.bin` lexical-parent table (design A §4.3) and the I5 filter:
//! a scoped node is reachable only through the lexical tier, so every
//! name-tier candidate source drops it, on the build side (`SymbolTable`)
//! and on the overlay side (`OverlayView`).
//!
//! Only Rust opts in (`lexical_scope_kinds(Rust)` is `[Function]`, slice R6),
//! so a real build writes rows for Rust files alone. The filter tests plant a
//! scoped node through the table API, so they hold for any language.

use ecp_analyzer::python::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::resolution::index::{FileMeta, GlobalPick, ResolveTarget, SymbolTable};
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_cli::engine::Engine;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{ArchivedZeroCopyGraph, LexicalParentFact, NodeKind, RelType, ZeroCopyGraph};
use ecp_core::graph_fixture::GraphFixture;
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::view::{OverlayFileInput, OverlayView};
use std::path::Path;

/// Each file nests a function in a function: the shape a language that opts
/// in would give a lexical parent.
const NESTED: [(&str, &str); 3] = [
    (
        "src/lib.rs",
        "fn outer() {\n    fn inner() {}\n    inner();\n}\n",
    ),
    (
        "app.py",
        "def outer():\n    def inner():\n        pass\n    inner()\n",
    ),
    (
        "web/util.ts",
        "function outer() {\n  function inner() {}\n  inner();\n}\n",
    ),
];

fn provider_for(path: &str) -> Box<dyn LanguageProvider> {
    match path.rsplit('.').next() {
        Some("rs") => Box::new(RustProvider::new().unwrap()),
        Some("py") => Box::new(PythonProvider::new().unwrap()),
        Some("ts") => Box::new(TypeScriptProvider::new().unwrap()),
        other => panic!("no provider for {other:?}"),
    }
}

fn build(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for &(path, source) in files {
        builder.add_graph(
            provider_for(path)
                .parse_file(Path::new(path), source.as_bytes())
                .unwrap(),
        );
    }
    builder.build()
}

fn archive(graph: &ZeroCopyGraph) -> rkyv::util::AlignedVec {
    rkyv::to_bytes::<rkyv::rancor::Error>(graph).unwrap()
}

fn access(bytes: &[u8]) -> &ArchivedZeroCopyGraph {
    rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).unwrap()
}

/// Stands for: a full build writes one row per scoped node of an opted-in
/// language, and a reader of the written `graph.bin` gets it back. The Rust
/// `inner` has the parent `outer` and prints the display owner `outer` with
/// an empty stored owner (I1); the Python and TypeScript `inner` opt out, so
/// they and every other node have no row and print the stored owner. Before
/// R6 the table was empty for all three files.
#[test]
fn test_graph_bin_lexical_parent_table_rust_only_row_round_trip() {
    let graph = build(&NESTED);
    let pool = graph.string_pool.as_slice();
    // Setup: each file holds an `inner` function inside `outer`, so the
    // fixture has nodes an opted-in language would scope.
    for &(path, _) in &NESTED {
        let file_idx = graph
            .files
            .iter()
            .position(|f| f.path.resolve(pool) == path)
            .unwrap_or_else(|| panic!("setup: {path} is indexed"));
        let span_of = |name: &str| {
            graph
                .nodes
                .iter()
                .find(|n| {
                    n.file_idx as usize == file_idx
                        && n.kind == NodeKind::Function
                        && n.name.resolve(pool) == name
                })
                .map(|n| n.span)
                .unwrap_or_else(|| panic!("setup: {path} declares `{name}`"))
        };
        let (outer, inner) = (span_of("outer"), span_of("inner"));
        assert!(
            (outer.0, outer.1) < (inner.0, inner.1) && (inner.2, inner.3) < (outer.2, outer.3),
            "setup: {path}'s `inner` lies inside `outer`"
        );
    }
    let in_file = |path: &str, name: &str| -> u32 {
        graph
            .nodes
            .iter()
            .position(|n| {
                n.kind == NodeKind::Function
                    && n.name.resolve(pool) == name
                    && graph.files[n.file_idx as usize].path.resolve(pool) == path
            })
            .unwrap_or_else(|| panic!("setup: {path} declares `{name}`")) as u32
    };
    let (rs_outer, rs_inner) = (
        in_file("src/lib.rs", "outer"),
        in_file("src/lib.rs", "inner"),
    );
    assert_eq!(
        graph
            .lexical_parents
            .iter()
            .map(|f| (f.node_idx, f.parent_idx))
            .collect::<Vec<_>>(),
        [(rs_inner, rs_outer)],
        "one row: the Rust `inner`"
    );

    let tmp = tempfile::tempdir().unwrap();
    let graph_path = tmp.path().join("graph.bin");
    std::fs::write(&graph_path, archive(&graph)).unwrap();
    let engine = Engine::load(&graph_path).expect("today's graph loads");
    let archived = engine.graph().unwrap();
    let pool = archived.string_pool.as_slice();
    assert_eq!(archived.nodes.len(), graph.nodes.len());
    assert_eq!(archived.lexical_parents.len(), 1);
    assert_eq!(archived.lexical_parent(rs_inner), Some(rs_outer));
    assert!(archived.is_lexically_scoped(rs_inner));
    assert_eq!(
        archived.nodes[rs_inner as usize].owner_class.resolve(pool),
        "",
        "a scoped node stores no owner"
    );
    assert_eq!(archived.display_owner(rs_inner).as_deref(), Some("outer"));
    for idx in (0..archived.nodes.len() as u32).filter(|&idx| idx != rs_inner) {
        assert_eq!(archived.lexical_parent(idx), None, "node {idx}");
        assert!(!archived.is_lexically_scoped(idx), "node {idx}");
        let owner = archived.nodes[idx as usize].owner_class.resolve(pool);
        assert_eq!(
            archived.display_owner(idx).as_deref(),
            (!owner.is_empty()).then_some(owner),
            "node {idx}: with no table row the display owner is the stored owner"
        );
    }
    assert_eq!(archived.lexical_parent(u32::MAX), None);
}

/// Stands for: a row given to the assembly reads back from `graph.bin` as
/// that node's parent, sorted, the first row of a repeated node kept, and
/// the display owner walks the parents (design A §4.5).
#[test]
fn test_lexical_parent_planted_rows_round_trip_returns_parent_and_display_owner() {
    let mut fx = GraphFixture::new();
    let outer = fx.func("src/a.rs", "outer");
    let helper = fx.func("src/a.rs", "helper");
    let new = fx.method("src/a.rs", "A", "new");
    let pad = fx.func("src/a.rs", "pad");
    let deep = fx.func("src/a.rs", "deep");
    // Out of order, and `helper` twice: `finish` sorts and keeps the first.
    fx.assembly_mut().lexical_parents = vec![
        LexicalParentFact {
            node_idx: pad,
            parent_idx: new,
        },
        LexicalParentFact {
            node_idx: deep,
            parent_idx: pad,
        },
        LexicalParentFact {
            node_idx: helper,
            parent_idx: outer,
        },
        LexicalParentFact {
            node_idx: helper,
            parent_idx: new,
        },
    ];
    let bytes = archive(&fx.build());
    let graph = access(&bytes);

    assert_eq!(graph.lexical_parents.len(), 3);
    assert_eq!(graph.lexical_parent(helper), Some(outer));
    assert_eq!(graph.lexical_parent(pad), Some(new));
    assert_eq!(graph.lexical_parent(deep), Some(pad));
    assert_eq!(graph.lexical_parent(outer), None);
    assert_eq!(graph.lexical_parent(new), None);
    assert!(graph.is_lexically_scoped(helper));
    assert!(!graph.is_lexically_scoped(outer));

    assert_eq!(graph.display_owner(outer), None);
    assert_eq!(graph.display_owner(new).as_deref(), Some("A"));
    // A parent with no owner prints its own name.
    assert_eq!(graph.display_owner(helper).as_deref(), Some("outer"));
    // A parent with an owner prints that owner, then its name.
    assert_eq!(graph.display_owner(pad).as_deref(), Some("A::new"));
    assert_eq!(graph.display_owner(deep).as_deref(), Some("A::new::pad"));
}

/// A table whose rows form a cycle (a corrupt graph) gives no display owner
/// rather than a loop.
#[test]
fn test_display_owner_cyclic_rows_returns_none() {
    let mut fx = GraphFixture::new();
    let a = fx.func("src/a.rs", "a");
    let b = fx.func("src/a.rs", "b");
    fx.assembly_mut().lexical_parents = vec![
        LexicalParentFact {
            node_idx: a,
            parent_idx: b,
        },
        LexicalParentFact {
            node_idx: b,
            parent_idx: a,
        },
    ];
    let bytes = archive(&fx.build());
    let graph = access(&bytes);
    assert_eq!(graph.display_owner(a), None);
    assert_eq!(graph.display_owner(b), None);
}

const APP_BASE: &str = "def run():\n    pass\n";
const APP_DIRTY: &str = "def run():\n    helper()\n";
const A_PY: &str = "def helper():\n    pass\n";
const B_PY: &str = "def outer():\n    pass\n\n\ndef helper():\n    pass\n";

/// `(file, name)` of every Calls target of `run` once the overlay replaces
/// `app.py` with [`APP_DIRTY`], with the confidence in percent.
fn overlay_run_targets(base: &ZeroCopyGraph) -> Vec<(String, String, u32)> {
    let dirty: OverlayFileInput = PythonProvider::new()
        .unwrap()
        .parse_file(Path::new("app.py"), APP_DIRTY.as_bytes())
        .unwrap()
        .into();
    let bytes = archive(base);
    let graph = access(&bytes);
    let view = OverlayView::build(graph, &[dirty]).unwrap();
    let merged = MergedGraph::new(graph, Some(&view));
    let mut targets: Vec<(String, String, u32)> = merged
        .all_edges()
        .filter(|e| {
            e.rel_type() == RelType::Calls && merged.node(e.source).unwrap().name(&merged) == "run"
        })
        .map(|e| {
            let n = merged.node(e.target).unwrap();
            (
                n.file_path(&merged).unwrap().to_string(),
                n.name(&merged).to_string(),
                (e.confidence() * 100.0).round() as u32,
            )
        })
        .collect();
    targets.sort();
    targets.dedup();
    targets
}

fn node_idx(graph: &ZeroCopyGraph, path: &str, name: &str) -> u32 {
    let pool = graph.string_pool.as_slice();
    graph
        .nodes
        .iter()
        .position(|n| {
            n.kind == NodeKind::Function
                && n.name.resolve(pool) == name
                && graph.files[n.file_idx as usize].path.resolve(pool) == path
        })
        .unwrap_or_else(|| panic!("setup: {path} declares `{name}`")) as u32
}

/// Stands for: I5 on both sides. A node the table marks scoped is no global
/// candidate: the build side's `SymbolTable` and the overlay's Tier 3 both
/// drop it, so its unscoped twin becomes the one candidate and binds. The
/// control arm, the same twins with no row, stays ambiguous on both sides:
/// that is what proves the filter, not the fixture, makes the difference.
#[test]
fn test_global_candidates_scoped_node_excluded_both_sides() {
    // Build side: ids in registration order, as Pass 1 numbers them.
    let caller = FileMeta::from_path("app.py");
    let table = |scoped: bool| {
        let mut st = SymbolTable::new();
        st.register_node("a.py", "helper", 0, NodeKind::Function);
        st.register_node("b.py", "outer", 1, NodeKind::Function);
        st.register_node("b.py", "helper", 2, NodeKind::Function);
        if scoped {
            st.note_lexical_parent(2, 1);
        }
        st
    };
    let control = table(false);
    assert_eq!(
        control.global_candidates("helper", ResolveTarget::Callable, caller),
        vec![0, 2]
    );
    for free_only in [false, true] {
        assert_eq!(
            control.lookup_global("helper", ResolveTarget::Callable, caller, free_only),
            GlobalPick::Ambiguous,
            "control: two unscoped `helper`s, free_only {free_only}"
        );
    }
    let scoped = table(true);
    assert!(scoped.is_lexically_scoped(2));
    assert!(!scoped.is_lexically_scoped(0));
    assert_eq!(
        scoped.global_candidates("helper", ResolveTarget::Callable, caller),
        vec![0],
        "the scoped `helper` is no global candidate; its twin still is"
    );
    for free_only in [false, true] {
        assert_eq!(
            scoped.lookup_global("helper", ResolveTarget::Callable, caller, free_only),
            GlobalPick::Unique(0),
            "free_only {free_only}"
        );
    }
    // The scoped node is in no file lookup of the name tiers either.
    assert_eq!(
        scoped.lookup_call_in_file("b.py", "helper", ResolveTarget::Callable, false),
        None
    );
    assert_eq!(
        control.lookup_call_in_file("b.py", "helper", ResolveTarget::Callable, false),
        Some(2)
    );

    // Overlay side: a base graph from a real build, the row planted.
    let files = [("app.py", APP_BASE), ("a.py", A_PY), ("b.py", B_PY)];
    let mut base = build(&files);
    assert!(base.lexical_parents.is_empty());
    assert_eq!(
        overlay_run_targets(&base),
        vec![],
        "control: `helper()` sees two unscoped twins and binds neither"
    );
    let b_helper = node_idx(&base, "b.py", "helper");
    let b_outer = node_idx(&base, "b.py", "outer");
    base.lexical_parents = vec![LexicalParentFact {
        node_idx: b_helper,
        parent_idx: b_outer,
    }];
    assert_eq!(
        overlay_run_targets(&base),
        vec![("a.py".to_string(), "helper".to_string(), 70)],
        "the scoped `b.py` helper is dropped; the unscoped twin binds at Tier 3"
    );
}
