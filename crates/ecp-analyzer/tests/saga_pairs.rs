//! Unit tests for the saga_pairs post-process detection helpers.

use ecp_analyzer::post_process::saga_pairs::{
    emit_edges, strip_compensator_root, CompensatorMatch,
};
use ecp_core::graph::{Edge, NodeKind, RelType};
use ecp_core::pool::StringPool;

fn method_node(pool: &mut StringPool, name: &str, owner: &str) -> ecp_core::graph::Node {
    ecp_core::graph::Node {
        uid: 0,
        name: pool.add(name),
        file_idx: 0,
        kind: NodeKind::Method,
        span: (1, 0, 1, 0),
        community_id: 0,
        owner_class: pool.add(owner),
        content_hash: 0,
    }
}

#[test]
fn test_emit_name_only_pair_confidence_0_6() {
    let mut pool = StringPool::new();
    let nodes = vec![
        method_node(&mut pool, "book_room", "OrderSaga"), // idx 0 = operation
        method_node(&mut pool, "undo_book_room", "OrderSaga"), // idx 1 = compensator
    ];
    let mut edges: Vec<Edge> = Vec::new();
    let count = emit_edges(&nodes, &mut pool, &mut edges);
    assert_eq!(count, 1, "one CompensatedBy edge expected");
    let e = &edges[0];
    assert_eq!(e.source, 1, "source = compensator idx");
    assert_eq!(e.target, 0, "target = operation idx");
    assert_eq!(e.rel_type, RelType::CompensatedBy);
    assert!((e.confidence - 0.6).abs() < 1e-6, "name-only -> 0.6");
    assert_eq!(pool.resolve(&e.reason), "saga:name-only");
}

#[test]
fn test_emit_calls_back_pair_confidence_0_8() {
    let mut pool = StringPool::new();
    let nodes = vec![
        method_node(&mut pool, "charge", "PaymentSaga"), // idx 0 = operation
        method_node(&mut pool, "rollback_charge", "PaymentSaga"), // idx 1 = compensator
    ];
    let mut edges = vec![Edge {
        source: 1,
        target: 0,
        rel_type: RelType::Calls,
        confidence: 1.0,
        reason: pool.add("call"),
    }];
    let count = emit_edges(&nodes, &mut pool, &mut edges);
    assert_eq!(count, 1);
    let e = edges.last().unwrap();
    assert_eq!(e.rel_type, RelType::CompensatedBy);
    assert!((e.confidence - 0.8).abs() < 1e-6, "calls-back -> 0.8");
    assert_eq!(pool.resolve(&e.reason), "saga:calls-back");
}

#[test]
fn test_emit_different_class_no_edge() {
    let mut pool = StringPool::new();
    let nodes = vec![
        method_node(&mut pool, "book_room", "OrderSaga"),
        method_node(&mut pool, "undo_book_room", "OtherSaga"),
    ];
    let mut edges: Vec<Edge> = Vec::new();
    assert_eq!(
        emit_edges(&nodes, &mut pool, &mut edges),
        0,
        "cross-class must not match"
    );
}

#[test]
fn test_emit_camel_and_pascal_case() {
    let mut pool = StringPool::new();
    let nodes = vec![
        method_node(&mut pool, "bookRoom", "Saga"),
        method_node(&mut pool, "undoBookRoom", "Saga"),
        method_node(&mut pool, "BookRoom", "Saga"),
        method_node(&mut pool, "UndoBookRoom", "Saga"),
    ];
    let mut edges: Vec<Edge> = Vec::new();
    assert_eq!(
        emit_edges(&nodes, &mut pool, &mut edges),
        2,
        "camel + pascal both match"
    );
}

#[test]
fn test_strip_root_snake_camel_pascal() {
    // snake_case
    assert_eq!(
        strip_compensator_root("undo_book_room"),
        Some(CompensatorMatch {
            operation_name: "book_room".to_string()
        })
    );
    // camelCase
    assert_eq!(
        strip_compensator_root("undoBookRoom"),
        Some(CompensatorMatch {
            operation_name: "bookRoom".to_string()
        })
    );
    // PascalCase
    assert_eq!(
        strip_compensator_root("UndoBookRoom"),
        Some(CompensatorMatch {
            operation_name: "BookRoom".to_string()
        })
    );
    // rollback / compensate roots
    assert_eq!(
        strip_compensator_root("rollback_charge"),
        Some(CompensatorMatch {
            operation_name: "charge".to_string()
        })
    );
    assert_eq!(
        strip_compensator_root("compensateReserve"),
        Some(CompensatorMatch {
            operation_name: "reserve".to_string()
        })
    );
    // non-compensator
    assert_eq!(strip_compensator_root("book_room"), None);
    // root but no suffix → not a pair
    assert_eq!(strip_compensator_root("undo"), None);
}
