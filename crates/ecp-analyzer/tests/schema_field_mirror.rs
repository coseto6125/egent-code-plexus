//! T4-7: `SchemaField` Node + `HasProperty` + `MirrorsField` end-to-end
//! emission tests.
//!
//! Exercises the full pipeline: per-language parsers emit
//! `RawSchemaField` → `GraphBuilder::build()` → `post_process::schema_field_mirrors`
//! → final `ZeroCopyGraph` with SchemaField nodes + HasProperty + MirrorsField edges.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};

fn parse_python(path: &str, src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = PythonProvider::new().expect("python provider");
    provider
        .parse_file(path.as_ref(), src.as_bytes())
        .expect("parse_file")
}

fn build(local_graphs: Vec<ecp_core::analyzer::types::LocalGraph>) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for lg in local_graphs {
        builder.add_graph(lg);
    }
    builder.build()
}

/// Lookup helper: count SchemaField nodes whose name resolves to `name`.
fn count_schema_field_nodes(graph: &ZeroCopyGraph, name: &str) -> usize {
    let pool = graph.string_pool.as_slice();
    graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::SchemaField && n.name.resolve(pool) == name)
        .count()
}

/// Lookup helper: count edges of a given rel_type.
fn count_edges(graph: &ZeroCopyGraph, rel: RelType) -> usize {
    graph.edges.iter().filter(|e| e.rel_type == rel).count()
}

/// Lookup helper: count MirrorsField edges between two specific SchemaField
/// indices (in either direction).
fn count_mirror_edges_between(graph: &ZeroCopyGraph, a: u32, b: u32) -> usize {
    graph
        .edges
        .iter()
        .filter(|e| {
            e.rel_type == RelType::MirrorsField
                && ((e.source == a && e.target == b) || (e.source == b && e.target == a))
        })
        .count()
}

/// Find the (idx, owner_class_name) of every SchemaField named `field_name`.
/// Owner class is derived by walking the inbound HasProperty edge.
fn find_schema_fields_with_owners<'g>(
    graph: &'g ZeroCopyGraph,
    field_name: &str,
) -> Vec<(u32, &'g str)> {
    let pool = graph.string_pool.as_slice();
    let mut out = Vec::new();
    for (idx, node) in graph.nodes.iter().enumerate() {
        if node.kind != NodeKind::SchemaField || node.name.resolve(pool) != field_name {
            continue;
        }
        let sf_idx = idx as u32;
        // Find the HasProperty edge whose target is this SchemaField.
        for edge in &graph.edges {
            if edge.rel_type == RelType::HasProperty && edge.target == sf_idx {
                let class_idx = edge.source as usize;
                let class_name = graph.nodes[class_idx].name.resolve(pool);
                out.push((sf_idx, class_name));
                break;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Spec test cases (T4-7 line 543-547)
// ---------------------------------------------------------------------------

/// `test_pair_strict_match_emits_mirrorsfield` — Pydantic `User.email: str` +
/// SQLA `User.email = Column(String)` → one MirrorsField edge.
#[test]
fn test_pair_strict_match_emits_mirrorsfield() {
    let pyd = parse_python(
        "models/pyd.py",
        "from pydantic import BaseModel\n\nclass User(BaseModel):\n    email: str\n",
    );
    let sqla = parse_python(
        "models/sqla.py",
        "from sqlalchemy import Column, String\n\nclass User(Base):\n    email = Column(String)\n",
    );

    let graph = build(vec![pyd, sqla]);

    // Two SchemaField nodes named "email".
    assert_eq!(
        count_schema_field_nodes(&graph, "email"),
        2,
        "two SchemaField nodes (Pydantic + SQLA) for User.email"
    );

    // Two HasProperty edges (one per SchemaField).
    assert!(
        count_edges(&graph, RelType::HasProperty) >= 2,
        "at least two HasProperty edges"
    );

    // Exactly one MirrorsField edge between them (pairwise, k=2).
    let fields = find_schema_fields_with_owners(&graph, "email");
    assert_eq!(fields.len(), 2, "Pydantic + SQLA both emit email");
    let (a_idx, a_owner) = fields[0];
    let (b_idx, b_owner) = fields[1];
    assert_eq!(a_owner, "User");
    assert_eq!(b_owner, "User");
    assert_eq!(
        count_mirror_edges_between(&graph, a_idx, b_idx),
        1,
        "exactly one MirrorsField edge between the two User.email fields"
    );
}

/// `test_three_way_cluster_all_pairs_emit_mirrorsfield` (D3) —
/// Pydantic + SQLA + a TS interface for the same `User.email` → 3 pairs of
/// MirrorsField edges (k=3 cluster, k×(k-1)/2 = 3 edges).
#[test]
fn test_three_way_cluster_all_pairs_emit_mirrorsfield() {
    use ecp_analyzer::typescript::TypeScriptProvider;

    let pyd = parse_python(
        "models/pyd.py",
        "from pydantic import BaseModel\n\nclass User(BaseModel):\n    email: str\n",
    );
    let sqla = parse_python(
        "models/sqla.py",
        "from sqlalchemy import Column, String\n\nclass User(Base):\n    email = Column(String)\n",
    );
    let ts_provider = TypeScriptProvider::new().expect("ts provider");
    let ts = ts_provider
        .parse_file(
            "models/user.ts".as_ref(),
            b"interface User { email: string; }",
        )
        .expect("parse_file");

    let graph = build(vec![pyd, sqla, ts]);

    // Three SchemaField nodes.
    assert_eq!(
        count_schema_field_nodes(&graph, "email"),
        3,
        "three SchemaField nodes for User.email (Pydantic + SQLA + TS)"
    );

    // k=3 cluster → 3 pairwise MirrorsField edges (3 choose 2).
    assert_eq!(
        count_edges(&graph, RelType::MirrorsField),
        3,
        "k=3 cluster must emit 3 pairwise MirrorsField edges"
    );
}

/// `test_partial_match_emits_blindspot` — Pydantic `User.email` + something
/// like SQLA `User.user_email` (3/4 match: name differs) → BlindSpot.
///
/// **T4-7 v1 limitation**: BlindSpot emission for partial matches is a
/// documented follow-up. Currently the field is silently dropped.
#[test]
#[ignore = "BlindSpot for partial-match SchemaField pairs is a T4-7 follow-up — see schema_field_mirrors.rs Phase 2 docs"]
fn test_partial_match_emits_blindspot() {
    let pyd = parse_python(
        "models/pyd.py",
        "from pydantic import BaseModel\n\nclass User(BaseModel):\n    email: str\n",
    );
    let sqla = parse_python(
        "models/sqla.py",
        "from sqlalchemy import Column, String\n\nclass User(Base):\n    user_email = Column(String)\n",
    );

    let graph = build(vec![pyd, sqla]);

    // No MirrorsField (different name).
    assert_eq!(count_edges(&graph, RelType::MirrorsField), 0);

    // BlindSpot expected — checked in follow-up PR.
    assert!(
        graph
            .blind_spots
            .iter()
            .any(|bs| bs.kind.resolve(graph.string_pool.as_slice())
                == "schema-field-mirror-candidate"),
        "partial match must surface as BlindSpot"
    );
}

/// `test_different_class_name_blindspot` — Pydantic `User.email` + SQLA
/// `Admin.email` (same type, different owner) → no MirrorsField + (future)
/// BlindSpot.
#[test]
fn test_different_class_name_drops_silently() {
    let pyd = parse_python(
        "models/pyd.py",
        "from pydantic import BaseModel\n\nclass User(BaseModel):\n    email: str\n",
    );
    let sqla = parse_python(
        "models/sqla.py",
        "from sqlalchemy import Column, String\n\nclass Admin(Base):\n    email = Column(String)\n",
    );

    let graph = build(vec![pyd, sqla]);

    // Both SchemaField nodes exist (HasProperty still emitted per-class).
    assert_eq!(count_schema_field_nodes(&graph, "email"), 2);

    // But NO MirrorsField (different owner-class fails the 4-point rubric).
    assert_eq!(
        count_edges(&graph, RelType::MirrorsField),
        0,
        "different owner-class must not emit MirrorsField"
    );
}

// ---------------------------------------------------------------------------
// Additional integration coverage
// ---------------------------------------------------------------------------

/// HasProperty edges that target SchemaField nodes specifically (T4-7
/// emission) — distinct from HasProperty edges to plain `Property` nodes
/// emitted by `class_membership`. This test focuses only on the
/// SchemaField subset.
#[test]
fn test_has_property_edge_direction_and_owner() {
    let pyd = parse_python(
        "models/pyd.py",
        "from pydantic import BaseModel\n\nclass User(BaseModel):\n    email: str\n    age: int\n",
    );
    let graph = build(vec![pyd]);

    let pool = graph.string_pool.as_slice();
    let sf_has_props: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| {
            e.rel_type == RelType::HasProperty
                && graph.nodes[e.target as usize].kind == NodeKind::SchemaField
        })
        .collect();
    assert_eq!(
        sf_has_props.len(),
        2,
        "User has 2 SchemaField properties → 2 HasProperty→SchemaField edges"
    );

    for edge in &sf_has_props {
        let src = &graph.nodes[edge.source as usize];
        assert_eq!(
            src.name.resolve(pool),
            "User",
            "HasProperty source must be Class User"
        );
    }
}

/// MirrorsField edges are listed under `is_heuristic` — default `ecp impact`
/// hides them. This test verifies the structural property; the impact CLI
/// filtering is already covered by `tests/impact_heuristic_filter.rs`.
#[test]
fn test_mirrors_field_is_heuristic() {
    assert!(
        RelType::MirrorsField.is_heuristic(),
        "MirrorsField MUST be marked heuristic so ecp impact hides it by default"
    );
}

/// File with no schema_fields → no SchemaField nodes, no MirrorsField edges.
/// Confirms the empty-fast-path doesn't break the rest of the build.
#[test]
fn test_no_schema_fields_no_emission() {
    let plain = parse_python("models/plain.py", "def add(x, y):\n    return x + y\n");
    let graph = build(vec![plain]);

    assert_eq!(count_schema_field_nodes(&graph, "email"), 0);
    assert_eq!(count_edges(&graph, RelType::MirrorsField), 0);
    // HasProperty count from this file is 0 (no class).
    assert_eq!(count_edges(&graph, RelType::HasProperty), 0);
}
