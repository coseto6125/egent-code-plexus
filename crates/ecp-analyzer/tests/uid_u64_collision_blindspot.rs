//! T1-5 D1 collision recovery: a merged repeat becomes a tombstone rather
//! than panicking or creating two nodes with the same UID.
//!
//! A merged repeat is forced by submitting two `RawNode` entries of a kind
//! that merges repeats (`uid::merges_repeats`) with identical (kind,
//! file_path, owner_class, name) — the four inputs to `uid::compute` — but
//! different span values. The second node must lose its name. In a C/C++
//! file the merge records an `ifdef-redef` BlindSpot; in any other file it
//! loses nothing (its edges start at the survivor) and records none. A
//! repeat of any other kind is a later node of its own (design uid §2.3) and
//! reports nothing. A true 64-bit collision with another path records
//! `uid-collision`; no fixture can force a 64-bit hash collision, so the
//! builder's unit test `test_classify_collision_true_collision_is_uid_collision`
//! covers that arm.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;

/// Two declarations of `kind` named `name` in `path`.
fn make_collision_graph(path: &str, kind: NodeKind, name: &str) -> LocalGraph {
    LocalGraph {
        file_path: path.into(),
        content_hash: [1; 8],
        nodes: vec![
            // First definition — kept.
            RawNode {
                name: name.to_string(),
                kind,
                span: (1, 0, 5, 0),
                is_exported: true,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
            },
            // Second definition — same (kind, path, owner, name) → same uid → collision.
            RawNode {
                name: name.to_string(),
                kind,
                span: (10, 0, 15, 0), // different span, same hash inputs
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
            },
        ],
        documents: vec![],
        imports: vec![],
        routes: vec![],
        route_receivers: None,
        framework_refs: vec![],
        fanout_refs: vec![],
        blind_spots: vec![],
        schema_fields: None,
        event_topics: None,
        tx_scopes: None,
        path_literals: None,
        sql_refs: None,
        call_metas: vec![],
        raw_function_metas: vec![],
        export_aliases: None,
        parsed_language: ecp_core::contracts::routing::ParsedLanguage::ABSENT,
    }
}

const PY: &str = "src/collision.py";

fn build(path: &str, kind: NodeKind, name: &str) -> ecp_core::graph::ZeroCopyGraph {
    let mut b = GraphBuilder::new();
    b.add_graph(make_collision_graph(path, kind, name));
    b.build()
}

fn kinds(g: &ecp_core::graph::ZeroCopyGraph) -> Vec<&str> {
    let pool = g.string_pool.as_slice();
    g.blind_spots
        .iter()
        .map(|bs| bs.kind.resolve(pool))
        .collect()
}

/// Stands for: a merged repeat keeps one named node; the second slot is a
/// tombstone with a uid of its own.
#[test]
fn collision_drops_second_node_no_panic() {
    let g = build(PY, NodeKind::Variable, "duplicateVar");
    let pool = g.string_pool.as_slice();

    let dup_nodes: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.name.resolve(pool) == "duplicateVar")
        .collect();
    assert_eq!(
        dup_nodes.len(),
        1,
        "expected exactly one 'duplicateVar' node, got {}",
        dup_nodes.len()
    );

    // No two nodes should share the same uid.
    let mut seen_uids = std::collections::HashSet::new();
    for node in g.nodes.iter() {
        assert!(
            seen_uids.insert(node.uid),
            "duplicate uid {} found in built graph",
            node.uid
        );
    }
}

/// Stands for: a merged repeat in a C file is an `#ifdef` twin, and the
/// merge records one `ifdef-redef` BlindSpot naming the symbol.
#[test]
fn test_build_c_merged_variable_twin_records_ifdef_redef() {
    let g = build("src/collision.c", NodeKind::Variable, "duplicateVar");
    let pool = g.string_pool.as_slice();
    let spots: Vec<_> = g
        .blind_spots
        .iter()
        .filter(|bs| bs.kind.resolve(pool) == "ifdef-redef")
        .collect();
    assert_eq!(spots.len(), 1, "all kinds: {:?}", kinds(&g));
    let hint = spots[0].hint.resolve(pool);
    assert!(hint.starts_with("ifdef-redef:"), "{hint}");
    assert!(hint.contains("duplicateVar"), "{hint}");
    assert!(!kinds(&g).contains(&"uid-collision"), "{:?}", kinds(&g));
}

/// Stands for: a merged repeat outside C/C++ (Python `x = 1; x = 2`) loses
/// no information, so it records no collision BlindSpot of any kind. Red
/// before 1d: it recorded `uid-collision`.
#[test]
fn test_build_python_merged_variable_records_no_blind_spot() {
    let g = build(PY, NodeKind::Variable, "duplicateVar");
    let kinds = kinds(&g);
    assert!(
        !kinds
            .iter()
            .any(|&k| ecp_core::graph::is_dev_metric_bs_kind(k)),
        "{kinds:?}"
    );
}

/// Stands for: a Python `def f` twice is a redefinition, a later node of
/// its own with its name under `overload(f, 1)`, so nothing is lost and no
/// `uid-collision` BlindSpot reports it. Red before 1b: the second `def`
/// was a tombstone with a BlindSpot.
#[test]
fn test_build_python_redefined_function_later_node_no_uid_collision() {
    let g = build(PY, NodeKind::Function, "f");
    let pool = g.string_pool.as_slice();
    let named: Vec<u64> = g
        .nodes
        .iter()
        .filter(|n| n.name.resolve(pool) == "f")
        .map(|n| n.uid)
        .collect();
    let f = ecp_core::uid::compute(NodeKind::Function, "src/collision.py", None, "f");
    assert_eq!(named, [f, ecp_core::uid::overload(f, 1)]);
    assert!(!kinds(&g).contains(&"uid-collision"), "{:?}", kinds(&g));
}

/// Stands for: on Unix `\` is a file-name character, so `x\y.py` is a file
/// apart from `x/y.py`. `Path` order puts `x0.py` between them; the build
/// keeps three files, `x\y.py` stored with its `\`, and its `def f` the named
/// first of its own file with one `File -> f` edge. Red before the Windows-only
/// separator rule: both `y.py` paths were stored `x/y.py`, the second `f` a
/// blank-named tombstone with a `uid-collision`, and `x/y.py`'s File node
/// defined `f` twice.
#[cfg(unix)]
#[test]
fn test_build_unix_backslash_file_name_beside_slash_path_three_files_no_uid_collision() {
    let paths = ["x/y.py", "x0.py", "x\\y.py"];
    let mut b = GraphBuilder::new();
    for path in paths {
        let mut graph = make_collision_graph(path, NodeKind::Function, "f");
        graph.nodes.truncate(1);
        b.add_graph(graph);
    }
    let g = b.build();
    let pool = g.string_pool.as_slice();
    let path_of = |file_idx: u32| g.files[file_idx as usize].path.resolve(pool);

    let mut stored: Vec<&str> = g.files.iter().map(|f| f.path.resolve(pool)).collect();
    stored.sort_unstable();
    assert_eq!(stored, ["x/y.py", "x0.py", "x\\y.py"]);

    let file_nodes: Vec<u32> = (0..g.nodes.len() as u32)
        .filter(|&i| g.nodes[i as usize].kind == NodeKind::File)
        .collect();
    assert_eq!(file_nodes.len(), 3);

    let fs: Vec<u32> = (0..g.nodes.len() as u32)
        .filter(|&i| g.nodes[i as usize].kind == NodeKind::Function)
        .collect();
    assert_eq!(fs.len(), 3, "one `f` per file, no tombstone");
    for &f in &fs {
        let node = &g.nodes[f as usize];
        let path = path_of(node.file_idx);
        assert_eq!(node.name.resolve(pool), "f", "{path}");
        assert_eq!(
            node.uid,
            ecp_core::uid::compute(NodeKind::Function, path, None, "f"),
            "{path}: the first `f` of its own file"
        );
    }

    let defines: Vec<(u32, u32)> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == ecp_core::graph::RelType::Defines)
        .filter(|e| file_nodes.contains(&e.source))
        .map(|e| (e.source, e.target))
        .collect();
    assert_eq!(defines.len(), 3, "{defines:?}");
    for (file, f) in &defines {
        assert_eq!(
            g.nodes[*file as usize].file_idx, g.nodes[*f as usize].file_idx,
            "a File node defines the `f` of its own file: {defines:?}"
        );
    }
    let mut targets: Vec<u32> = defines.iter().map(|&(_, f)| f).collect();
    targets.sort_unstable();
    assert_eq!(targets, fs, "one `File -> f` per file");

    assert!(!kinds(&g).contains(&"uid-collision"), "{:?}", kinds(&g));
}
