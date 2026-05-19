//! `pub` prefix visibility checks for the Zig provider.
//!
//! In Zig, declarations preceded by the `pub` keyword are externally visible.
//! The provider detects this via a 4-byte prefix scan (`b"pub "`) at the
//! declaration's start byte in the source buffer.

use graph_nexus_analyzer::zig::parser::ZigProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    ZigProvider::new()
        .expect("ZigProvider init")
        .parse_file(Path::new("test.zig"), src.as_bytes())
        .expect("parse_file")
}

fn find_exported(g: &LocalGraph, name: &str) -> Option<bool> {
    g.nodes.iter().find(|n| n.name == name).map(|n| n.is_exported)
}

#[test]
fn pub_fn_is_exported() {
    let g = parse("pub fn pub_fn() void {}");
    assert_eq!(find_exported(&g, "pub_fn"), Some(true), "`pub_fn` must be exported");
}

#[test]
fn private_fn_is_not_exported() {
    let g = parse("fn priv_fn() void {}");
    assert_eq!(find_exported(&g, "priv_fn"), Some(false), "`priv_fn` must NOT be exported");
}

#[test]
fn pub_const_is_exported() {
    let g = parse("pub const PubConst = 42;");
    assert_eq!(find_exported(&g, "PubConst"), Some(true), "`PubConst` must be exported");
}

#[test]
fn private_const_is_not_exported() {
    let g = parse("const PrivConst = 42;");
    assert_eq!(find_exported(&g, "PrivConst"), Some(false), "`PrivConst` must NOT be exported");
}
