//! Visibility checks for the Crystal provider.
//!
//! Crystal methods are public by default. An inline `private` or `protected`
//! keyword immediately before the `def` marks the definition as non-exported.

use graph_nexus_analyzer::crystal::parser::CrystalProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    CrystalProvider::new()
        .expect("provider")
        .parse_file(Path::new("test.cr"), src.as_bytes())
        .expect("parse")
}

fn find_exported(g: &LocalGraph, name: &str) -> Option<bool> {
    g.nodes.iter().find(|n| n.name == name).map(|n| n.is_exported)
}

#[test]
fn public_method_in_class_is_exported() {
    let g = parse("class Foo; def pub_method; end; end");
    assert_eq!(
        find_exported(&g, "pub_method"),
        Some(true),
        "`pub_method` must be exported (default public)"
    );
}

#[test]
fn private_method_is_not_exported() {
    let g = parse("class Foo\n  private def priv_method\n  end\nend");
    assert_eq!(
        find_exported(&g, "priv_method"),
        Some(false),
        "`priv_method` preceded by `private` must not be exported"
    );
}

#[test]
fn protected_method_is_not_exported() {
    let g = parse("class Foo\n  protected def prot_method\n  end\nend");
    assert_eq!(
        find_exported(&g, "prot_method"),
        Some(false),
        "`prot_method` preceded by `protected` must not be exported"
    );
}

#[test]
fn top_level_def_is_exported() {
    let g = parse("def top_level_fn; end");
    assert_eq!(
        find_exported(&g, "top_level_fn"),
        Some(true),
        "top-level `def` must be exported (default public)"
    );
}
