//! Graph-level 14-lang parity test for `Node.owner_class` (T1-11 follow-up).
//!
//! Exercises the full builder pipeline:
//! `LocalGraph` → `GraphBuilder::build()` → `ZeroCopyGraph` — verifying that
//! `RawNode.owner_class` (`Option<String>`) survives the sentinel-StrRef
//! promotion on the final `Node` struct that Cypher / rename consumers see.
//!
//! Each test feeds one minimal `LocalGraph` through `GraphBuilder`, then
//! inspects `ZeroCopyGraph.nodes` and asserts:
//!  - the method node has a non-empty `owner_class` resolving to the class
//!  - the module-level function has an empty `owner_class` (sentinel: len==0)
//!
//! Required by CLAUDE.md "Parser / core-feature changes require 14-language
//! coverage" — guards against future per-language regressions in the
//! `RawNode → Node` owner_class promotion path.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::{NodeKind, ZeroCopyGraph};

fn raw_node(name: &str, kind: NodeKind, owner_class: Option<&str>) -> RawNode {
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
        owner_class: owner_class.map(str::to_string),
        content_hash: 0,
    }
}

fn local_graph(file: &str, nodes: Vec<RawNode>) -> LocalGraph {
    LocalGraph {
        file_path: file.into(),
        nodes,
        ..Default::default()
    }
}

fn build(lg: LocalGraph) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    builder.build()
}

/// Resolve `owner_class` of the node named `method_name` in `g`.
/// Returns `None` when sentinel (empty resolve), else `Some(class_name)`.
fn resolve_owner(g: &ZeroCopyGraph, method_name: &str) -> Option<String> {
    let pool = g.string_pool.as_slice();
    g.nodes.iter().find_map(|n| {
        if n.name.resolve(pool) == method_name {
            let oc = n.owner_class.resolve(pool);
            (!oc.is_empty()).then(|| oc.to_string())
        } else {
            None
        }
    })
}

/// True iff `fn_name`'s node has the sentinel (empty) owner_class.
fn owner_is_sentinel(g: &ZeroCopyGraph, fn_name: &str) -> bool {
    let pool = g.string_pool.as_slice();
    g.nodes
        .iter()
        .any(|n| n.name.resolve(pool) == fn_name && n.owner_class.resolve(pool).is_empty())
}

// ─── TypeScript ───────────────────────────────────────────────────────────────
#[test]
fn ts_node_owner_class_promoted_to_graph() {
    let g = build(local_graph(
        "src/app.ts",
        vec![
            raw_node("Dog", NodeKind::Class, None),
            raw_node("bark", NodeKind::Method, Some("Dog")),
            raw_node("freeFn", NodeKind::Function, None),
        ],
    ));
    assert_eq!(
        resolve_owner(&g, "bark").as_deref(),
        Some("Dog"),
        "TS: bark must own Dog"
    );
    assert!(
        owner_is_sentinel(&g, "freeFn"),
        "TS: freeFn must have sentinel owner_class"
    );
}

// ─── JavaScript ───────────────────────────────────────────────────────────────
#[test]
fn js_node_owner_class_promoted_to_graph() {
    let g = build(local_graph(
        "src/app.js",
        vec![
            raw_node("Dog", NodeKind::Class, None),
            raw_node("bark", NodeKind::Method, Some("Dog")),
            raw_node("freeFn", NodeKind::Function, None),
        ],
    ));
    assert_eq!(
        resolve_owner(&g, "bark").as_deref(),
        Some("Dog"),
        "JS: bark must own Dog"
    );
    assert!(
        owner_is_sentinel(&g, "freeFn"),
        "JS: freeFn must have sentinel owner_class"
    );
}

// ─── Python ──────────────────────────────────────────────────────────────────
#[test]
fn python_node_owner_class_promoted_to_graph() {
    let g = build(local_graph(
        "src/app.py",
        vec![
            raw_node("Dog", NodeKind::Class, None),
            raw_node("bark", NodeKind::Method, Some("Dog")),
            raw_node("free_fn", NodeKind::Function, None),
        ],
    ));
    assert_eq!(
        resolve_owner(&g, "bark").as_deref(),
        Some("Dog"),
        "Py: bark must own Dog"
    );
    assert!(
        owner_is_sentinel(&g, "free_fn"),
        "Py: free_fn must have sentinel owner_class"
    );
}

// ─── Java ─────────────────────────────────────────────────────────────────────
#[test]
fn java_node_owner_class_promoted_to_graph() {
    let g = build(local_graph(
        "src/Dog.java",
        vec![
            raw_node("Dog", NodeKind::Class, None),
            raw_node("bark", NodeKind::Method, Some("Dog")),
            raw_node("freeFn", NodeKind::Function, None),
        ],
    ));
    assert_eq!(
        resolve_owner(&g, "bark").as_deref(),
        Some("Dog"),
        "Java: bark must own Dog"
    );
    assert!(
        owner_is_sentinel(&g, "freeFn"),
        "Java: freeFn must have sentinel owner_class"
    );
}

// ─── Kotlin ───────────────────────────────────────────────────────────────────
#[test]
fn kotlin_node_owner_class_promoted_to_graph() {
    let g = build(local_graph(
        "src/Dog.kt",
        vec![
            raw_node("Dog", NodeKind::Class, None),
            raw_node("bark", NodeKind::Method, Some("Dog")),
            raw_node("topLevel", NodeKind::Function, None),
        ],
    ));
    assert_eq!(
        resolve_owner(&g, "bark").as_deref(),
        Some("Dog"),
        "Kotlin: bark must own Dog"
    );
    assert!(
        owner_is_sentinel(&g, "topLevel"),
        "Kotlin: topLevel must have sentinel owner_class"
    );
}

// ─── C# ───────────────────────────────────────────────────────────────────────
#[test]
fn csharp_node_owner_class_promoted_to_graph() {
    let g = build(local_graph(
        "src/Dog.cs",
        vec![
            raw_node("Dog", NodeKind::Class, None),
            raw_node("Bark", NodeKind::Method, Some("Dog")),
            raw_node("FreeFunc", NodeKind::Function, None),
        ],
    ));
    assert_eq!(
        resolve_owner(&g, "Bark").as_deref(),
        Some("Dog"),
        "C#: Bark must own Dog"
    );
    assert!(
        owner_is_sentinel(&g, "FreeFunc"),
        "C#: FreeFunc must have sentinel owner_class"
    );
}

// ─── Go ───────────────────────────────────────────────────────────────────────
#[test]
fn go_node_owner_class_promoted_to_graph() {
    let g = build(local_graph(
        "src/dog.go",
        vec![
            raw_node("Dog", NodeKind::Struct, None),
            raw_node("Bark", NodeKind::Method, Some("Dog")),
            raw_node("FreeFunc", NodeKind::Function, None),
        ],
    ));
    assert_eq!(
        resolve_owner(&g, "Bark").as_deref(),
        Some("Dog"),
        "Go: Bark must own Dog"
    );
    assert!(
        owner_is_sentinel(&g, "FreeFunc"),
        "Go: FreeFunc must have sentinel owner_class"
    );
}

// ─── Rust ─────────────────────────────────────────────────────────────────────
#[test]
fn rust_node_owner_class_promoted_to_graph() {
    let g = build(local_graph(
        "src/dog.rs",
        vec![
            raw_node("Dog", NodeKind::Struct, None),
            raw_node("bark", NodeKind::Method, Some("Dog")),
            raw_node("free_fn", NodeKind::Function, None),
        ],
    ));
    assert_eq!(
        resolve_owner(&g, "bark").as_deref(),
        Some("Dog"),
        "Rust: bark must own Dog"
    );
    assert!(
        owner_is_sentinel(&g, "free_fn"),
        "Rust: free_fn must have sentinel owner_class"
    );
}

// ─── PHP ─────────────────────────────────────────────────────────────────────
#[test]
fn php_node_owner_class_promoted_to_graph() {
    let g = build(local_graph(
        "src/Dog.php",
        vec![
            raw_node("Dog", NodeKind::Class, None),
            raw_node("bark", NodeKind::Method, Some("Dog")),
            raw_node("freeFn", NodeKind::Function, None),
        ],
    ));
    assert_eq!(
        resolve_owner(&g, "bark").as_deref(),
        Some("Dog"),
        "PHP: bark must own Dog"
    );
    assert!(
        owner_is_sentinel(&g, "freeFn"),
        "PHP: freeFn must have sentinel owner_class"
    );
}

// ─── Ruby ─────────────────────────────────────────────────────────────────────
#[test]
fn ruby_node_owner_class_promoted_to_graph() {
    let g = build(local_graph(
        "src/dog.rb",
        vec![
            raw_node("Dog", NodeKind::Class, None),
            raw_node("bark", NodeKind::Method, Some("Dog")),
            raw_node("free_fn", NodeKind::Function, None),
        ],
    ));
    assert_eq!(
        resolve_owner(&g, "bark").as_deref(),
        Some("Dog"),
        "Ruby: bark must own Dog"
    );
    assert!(
        owner_is_sentinel(&g, "free_fn"),
        "Ruby: free_fn must have sentinel owner_class"
    );
}

// ─── Swift ────────────────────────────────────────────────────────────────────
#[test]
fn swift_node_owner_class_promoted_to_graph() {
    let g = build(local_graph(
        "src/Dog.swift",
        vec![
            raw_node("Dog", NodeKind::Class, None),
            raw_node("bark", NodeKind::Method, Some("Dog")),
            raw_node("freeFunc", NodeKind::Function, None),
        ],
    ));
    assert_eq!(
        resolve_owner(&g, "bark").as_deref(),
        Some("Dog"),
        "Swift: bark must own Dog"
    );
    assert!(
        owner_is_sentinel(&g, "freeFunc"),
        "Swift: freeFunc must have sentinel owner_class"
    );
}

// ─── C ────────────────────────────────────────────────────────────────────────
/// C lacks first-class methods, but parsers may tag naming-convention
/// functions with `owner_class` (e.g. `Dog_bark` → owner `Dog`).
#[test]
fn c_node_owner_class_promoted_to_graph() {
    let g = build(local_graph(
        "src/dog.c",
        vec![
            raw_node("Dog", NodeKind::Struct, None),
            raw_node("Dog_bark", NodeKind::Function, Some("Dog")),
            raw_node("free_fn", NodeKind::Function, None),
        ],
    ));
    assert_eq!(
        resolve_owner(&g, "Dog_bark").as_deref(),
        Some("Dog"),
        "C: Dog_bark must own Dog"
    );
    assert!(
        owner_is_sentinel(&g, "free_fn"),
        "C: free_fn must have sentinel owner_class"
    );
}

// ─── C++ ──────────────────────────────────────────────────────────────────────
#[test]
fn cpp_node_owner_class_promoted_to_graph() {
    let g = build(local_graph(
        "src/dog.cpp",
        vec![
            raw_node("Dog", NodeKind::Class, None),
            raw_node("bark", NodeKind::Method, Some("Dog")),
            raw_node("freeFunc", NodeKind::Function, None),
        ],
    ));
    assert_eq!(
        resolve_owner(&g, "bark").as_deref(),
        Some("Dog"),
        "C++: bark must own Dog"
    );
    assert!(
        owner_is_sentinel(&g, "freeFunc"),
        "C++: freeFunc must have sentinel owner_class"
    );
}

// ─── Dart ─────────────────────────────────────────────────────────────────────
#[test]
fn dart_node_owner_class_promoted_to_graph() {
    let g = build(local_graph(
        "src/dog.dart",
        vec![
            raw_node("Dog", NodeKind::Class, None),
            raw_node("bark", NodeKind::Method, Some("Dog")),
            raw_node("freeFn", NodeKind::Function, None),
        ],
    ));
    assert_eq!(
        resolve_owner(&g, "bark").as_deref(),
        Some("Dog"),
        "Dart: bark must own Dog"
    );
    assert!(
        owner_is_sentinel(&g, "freeFn"),
        "Dart: freeFn must have sentinel owner_class"
    );
}

// ─── Round-trip: None RawNode owner_class becomes sentinel StrRef ─────────────
#[test]
fn none_raw_owner_becomes_sentinel_after_build() {
    let g = build(local_graph(
        "src/util.rs",
        vec![raw_node("standalone", NodeKind::Function, None)],
    ));
    assert!(
        owner_is_sentinel(&g, "standalone"),
        "standalone (RawNode.owner_class=None) must promote to sentinel StrRef"
    );
}
