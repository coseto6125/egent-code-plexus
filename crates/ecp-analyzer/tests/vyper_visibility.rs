//! Decorator-driven visibility checks for the Vyper provider.
//!
//! In Vyper, a function is externally callable when decorated with
//! `@external`, `@view`, or `@payable`. Functions decorated only with
//! `@internal`, `@pure`, or `@nonreentrant` are NOT externally callable.
//! Bare functions (no decorator) are also not exported.

use ecp_analyzer::vyper::parser::VyperProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    VyperProvider::new()
        .expect("VyperProvider init")
        .parse_file(Path::new("test.vy"), src.as_bytes())
        .expect("parse_file")
}

fn find_exported(g: &LocalGraph, name: &str) -> Option<bool> {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .map(|n| n.is_exported)
}

#[test]
fn external_fn_is_exported() {
    let g = parse("@external\ndef pub_fn():\n    pass\n");
    assert_eq!(
        find_exported(&g, "pub_fn"),
        Some(true),
        "`pub_fn` must be exported"
    );
}

#[test]
fn view_fn_is_exported() {
    let g = parse("@view\ndef view_fn() -> uint256:\n    return 0\n");
    assert_eq!(
        find_exported(&g, "view_fn"),
        Some(true),
        "`view_fn` must be exported"
    );
}

#[test]
fn internal_fn_is_not_exported() {
    let g = parse("@internal\ndef priv_fn():\n    pass\n");
    assert_eq!(
        find_exported(&g, "priv_fn"),
        Some(false),
        "`priv_fn` must NOT be exported"
    );
}

#[test]
fn external_view_fn_is_exported() {
    let g = parse("@external\n@view\ndef both():\n    pass\n");
    assert_eq!(
        find_exported(&g, "both"),
        Some(true),
        "`both` (external+view) must be exported"
    );
}

#[test]
fn bare_fn_is_not_exported() {
    let g = parse("def plain():\n    pass\n");
    assert_eq!(
        find_exported(&g, "plain"),
        Some(false),
        "bare `plain` must NOT be exported"
    );
}
