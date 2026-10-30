//! `RustProvider` on the shared driver, for the inputs the corpus dump may
//! not hold. Each test pins the records the hand-written loop produced
//! before the move, in their emitted order where the order is fixed by
//! source position.

mod driver_parity_support;

use driver_parity_support::{assert_empty_graph, names_kinds, refs};
use ecp_analyzer::framework_confidence::{ACTIX_ROUTE, AXUM_ROUTE};
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{FrameworkId, LocalGraph};
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &[u8]) -> LocalGraph {
    RustProvider::new()
        .expect("RustProvider::new")
        .parse_file(Path::new("src/app.rs"), src)
        .expect("parse_file")
}

fn imports(g: &LocalGraph) -> Vec<(&str, &str, Option<&str>)> {
    g.imports
        .iter()
        .map(|i| {
            (
                i.source.as_str(),
                i.imported_name.as_str(),
                i.alias.as_deref(),
            )
        })
        .collect()
}

#[test]
fn test_rust_driver_zero_byte_file_returns_empty_graph() {
    let g = parse(b"");
    assert_empty_graph(&g, "src/app.rs");
}

/// The query imports come first in source order; the nested-use walk then
/// appends its items, visiting the `use` items last to first.
#[test]
fn test_rust_driver_use_only_file_keeps_query_then_nested_import_order() {
    let g = parse(b"use a::b::C;\nuse d::{e, f::G, h::{i}};\nuse l::*;\n");
    assert!(g.nodes.is_empty(), "{:?}", names_kinds(&g));
    assert_eq!(
        imports(&g),
        [
            ("a::b", "C", None),
            ("d", "e", None),
            ("l", "*", None),
            ("d::f", "G", None),
            ("d::h", "i", None),
        ]
    );
    assert!(g.imports.iter().all(|i| i.binding_kind.is_none()));
}

/// A tuple self type has no name, so the `impl` emits no node; its method
/// and the file's other records stay.
#[test]
fn test_rust_driver_impl_without_self_type_name_emits_no_impl_node() {
    let g = parse(b"use x::y;\nimpl Foo for (u8, u8) {\n    fn m(&self) {}\n}\n");
    assert_eq!(names_kinds(&g), [("m", NodeKind::Method)]);
    assert_eq!(imports(&g), [("x", "y", None)]);
}

/// An attribute whose bytes are not UTF-8 is not recorded as a decorator;
/// the function keeps its node and its hash covers the raw bytes.
#[test]
fn test_rust_driver_non_utf8_region_skips_decorator_keeps_node() {
    let src: &[u8] = b"#[doc = \"\xff\"]\nfn ok() { let s = \"\xfe\"; }\n";
    let g = parse(src);
    assert_eq!(names_kinds(&g), [("ok", NodeKind::Function)]);
    let n = &g.nodes[0];
    assert!(n.decorators.is_empty(), "{:?}", n.decorators);
    assert_eq!(n.span, (1, 0, 1, 24));
    let item = &src[13..src.len() - 1];
    assert_eq!(n.content_hash, ecp_core::uid::xxh3_64_bytes(item));
}

#[test]
fn test_rust_driver_transaction_on_method_binds_scope_to_method() {
    let g = parse(
        b"struct Repo;\nimpl Repo {\n    #[transaction]\n    fn save(&self) {}\n    fn load(&self) {}\n}\n",
    );
    let scopes = g.tx_scopes.as_deref().expect("one tx scope");
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].framework(), FrameworkId::RustTransaction);
    let save = &g.nodes[scopes[0].node_idx() as usize];
    assert_eq!((save.name.as_str(), save.kind), ("save", NodeKind::Method));
    assert_eq!(save.decorators, ["#[transaction]"]);
    assert_eq!(save.owner_class.as_deref(), Some("Repo"));
    let load = g.nodes.iter().find(|n| n.name == "load").expect("load");
    assert!(load.decorators.is_empty());
}

#[test]
fn test_rust_driver_axum_route_emits_ref_from_enclosing_fn() {
    let body = "async fn handler() {}\nfn app() -> Router {\n    Router::new().route(\"/x\", get(handler))\n}\n";
    let g = parse(format!("use axum::Router;\n{body}").as_bytes());
    assert_eq!(
        refs(&g),
        [(
            "app",
            "handler",
            "axum-route-handler",
            AXUM_ROUTE,
            (3, 34, 3, 41)
        )]
    );
    // Without an `axum` import the gate drops the ref.
    assert!(parse(body.as_bytes()).framework_refs.is_empty());
}

#[test]
fn test_rust_driver_actix_route_emits_module_level_ref() {
    let g = parse(b"use actix_web::get;\n#[get(\"/x\")]\nasync fn index() {}\n");
    assert_eq!(
        refs(&g),
        [(
            "<module>",
            "index",
            "actix-route-get",
            ACTIX_ROUTE,
            (2, 9, 2, 14)
        )]
    );
    assert_eq!(names_kinds(&g), [("index", NodeKind::Function)]);
    assert_eq!(g.nodes[0].decorators, ["#[get(\"/x\")]"]);
}
