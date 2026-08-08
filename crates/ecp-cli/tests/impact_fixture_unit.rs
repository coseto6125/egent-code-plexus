//! Fixture-based unit tests for `impact`'s mode logic, driven directly
//! against `build_payload` / `run_for_symbol` — no `git init`, no
//! `ecp admin index` subprocess, no CLI binary spawn.
//!
//! Mirrors the synthetic-graph pattern in `impact_heuristic_filter.rs` and
//! `ecp_core::cypher::executor`'s `build_two_node` / `build_three_chain`
//! fixture helpers, but loads the serialized graph via `Engine::load` from
//! a tempdir file instead of driving it through the `ecp` binary.

use ecp_cli::commands::impact::{run_for_symbol, ImpactArgs};
use ecp_cli::engine::Engine;
use ecp_core::graph::RelType;
use ecp_core::graph_fixture::GraphFixture;

/// `entry(0) -[:Calls]-> middle(1) -[:Calls]-> leaf(2)`, one node per file.
/// Direction `Up` from `leaf` reaches `middle` then `entry`; `Down` from
/// `entry` reaches `middle` then `leaf`.
fn build_chain_graph() -> Vec<u8> {
    let mut fx = GraphFixture::new();
    let entry = fx.func("src/entry.ts", "entry");
    fx.span(entry, (0, 0, 2, 0));
    let middle = fx.func("src/middle.ts", "middle");
    fx.span(middle, (0, 0, 2, 0));
    let leaf = fx.func("src/leaf.ts", "leaf");
    fx.span(leaf, (0, 0, 2, 0));

    // entry(0) -> middle(1) -> leaf(2)
    fx.edge_with(entry, middle, RelType::Calls, 1.0, "call");
    fx.edge_with(middle, leaf, RelType::Calls, 1.0, "call");
    fx.into_bytes()
}

/// Two disconnected nodes both named `dup`, in different files — a caller
/// with a plain `dup` positional name cannot disambiguate without --file /
/// --kind.
fn build_ambiguous_graph() -> Vec<u8> {
    let mut fx = GraphFixture::new();
    let a = fx.func("src/a.ts", "dup");
    fx.span(a, (0, 0, 2, 0));
    let b = fx.func("src/b.ts", "dup");
    fx.span(b, (0, 0, 2, 0));
    fx.into_bytes()
}

fn engine_from_bytes(bytes: &[u8]) -> (tempfile::TempDir, Engine) {
    let tmp = tempfile::tempdir().unwrap();
    let graph_path = tmp.path().join("graph.bin");
    std::fs::write(&graph_path, bytes).unwrap();
    let engine = Engine::load(&graph_path).expect("engine load from tempdir graph.bin");
    (tmp, engine)
}

fn base_args(name: &str) -> ImpactArgs {
    ImpactArgs {
        name: Some(name.to_string()),
        target: None,
        baseline: None,
        file: None,
        kind: None,
        direction: ecp_cli::commands::impact::Direction::Up,
        depth: 5,
        high_trust_only: false,
        min_confidence: None,
        include_tests: false,
        relation_types: None,
        repo: None,
        test_coverage: false,
        no_heuristic: false,
        confidence_threshold: ecp_cli::commands::impact::DEFAULT_CONFIDENCE_THRESHOLD,
        explain_confidence: false,
        format: None,
        literal: None,
        literal_coherence: false,
        batch: false,
    }
}

#[test]
fn test_build_payload_symbol_mode_caller_chain_has_depth_and_file_fields() {
    let (_tmp, engine) = engine_from_bytes(&build_chain_graph());
    let mut args = base_args("leaf");
    args.direction = ecp_cli::commands::impact::Direction::Up;

    let payload = ecp_cli::commands::impact::build_payload(&args, &engine)
        .expect("build_payload should resolve `leaf` unambiguously");

    assert_eq!(payload["status"], "success");
    assert_eq!(payload["target"], "leaf");
    let impact = payload["impact"]
        .as_array()
        .expect("`impact` must be an array");
    assert!(
        impact.len() >= 3,
        "expected leaf(depth0) + middle(depth1) + entry(depth2), got {impact:?}"
    );

    for entry in impact {
        assert!(entry["depth"].is_u64(), "missing depth field: {entry}");
        assert!(entry["filePath"].is_string(), "missing filePath: {entry}");
        assert!(entry["name"].is_string(), "missing name: {entry}");
    }

    let names: Vec<&str> = impact.iter().filter_map(|e| e["name"].as_str()).collect();
    assert!(names.contains(&"leaf"));
    assert!(names.contains(&"middle"));
    assert!(names.contains(&"entry"));
}

#[test]
fn test_build_payload_ambiguous_name_without_disambiguator_returns_error() {
    let (_tmp, engine) = engine_from_bytes(&build_ambiguous_graph());
    let args = base_args("dup");

    let err = ecp_cli::commands::impact::build_payload(&args, &engine)
        .expect_err("two same-named `dup` defs with no --file/--kind must be ambiguous");

    assert!(
        matches!(err, ecp_core::EcpError::AmbiguousSymbol { count: 2, .. }),
        "expected AmbiguousSymbol{{count: 2}}, got {err:?}"
    );
}

#[test]
fn test_build_payload_ambiguous_name_with_file_disambiguator_succeeds() {
    let (_tmp, engine) = engine_from_bytes(&build_ambiguous_graph());
    let mut args = base_args("dup");
    args.file = Some("a.ts".to_string());

    let payload = ecp_cli::commands::impact::build_payload(&args, &engine)
        .expect("--file narrows to a single `dup` definition");

    assert_eq!(payload["status"], "success");
    let impact = payload["impact"].as_array().unwrap();
    assert_eq!(impact[0]["filePath"], "src/a.ts");
}

#[test]
fn test_build_payload_direction_up_vs_down_produce_different_results() {
    let (_tmp, engine) = engine_from_bytes(&build_chain_graph());

    let mut up_args = base_args("middle");
    up_args.direction = ecp_cli::commands::impact::Direction::Up;
    let up_payload = ecp_cli::commands::impact::build_payload(&up_args, &engine).unwrap();
    let up_names: Vec<String> = up_payload["impact"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str().map(str::to_string))
        .collect();

    let mut down_args = base_args("middle");
    down_args.direction = ecp_cli::commands::impact::Direction::Down;
    let down_payload = ecp_cli::commands::impact::build_payload(&down_args, &engine).unwrap();
    let down_names: Vec<String> = down_payload["impact"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str().map(str::to_string))
        .collect();

    assert!(up_names.contains(&"entry".to_string()));
    assert!(!up_names.contains(&"leaf".to_string()));

    assert!(down_names.contains(&"leaf".to_string()));
    assert!(!down_names.contains(&"entry".to_string()));

    assert_ne!(up_names, down_names);
}

#[test]
fn test_build_payload_depth_limit_stops_traversal_before_full_chain() {
    let (_tmp, engine) = engine_from_bytes(&build_chain_graph());
    let mut args = base_args("leaf");
    args.direction = ecp_cli::commands::impact::Direction::Up;
    args.depth = 1;

    let payload = ecp_cli::commands::impact::build_payload(&args, &engine).unwrap();
    let names: Vec<&str> = payload["impact"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();

    // depth=1 from `leaf` (Up): leaf itself (depth 0) + middle (depth 1).
    // `entry` sits at depth 2 and must NOT be reached.
    assert!(names.contains(&"leaf"));
    assert!(names.contains(&"middle"));
    assert!(
        !names.contains(&"entry"),
        "depth=1 BFS must not reach `entry` at depth 2: {names:?}"
    );
}

#[test]
fn test_run_for_symbol_wraps_build_payload_and_exposes_direct_symbol_uids() {
    let (_tmp, engine) = engine_from_bytes(&build_chain_graph());

    let local = run_for_symbol(
        &engine,
        "member-repo",
        "leaf",
        None,
        "up",
        Some(5),
        None,
        false,
    )
    .expect("run_for_symbol should resolve `leaf`");

    assert!(
        local.direct_count() >= 2,
        "expected middle + entry as callers, got direct_count={}",
        local.direct_count()
    );
    let uids = local.direct_symbol_uids();
    assert_eq!(
        uids.len(),
        local.as_json()["impact"].as_array().unwrap().len(),
        "direct_symbol_uids length must match the raw impact array length"
    );
}
