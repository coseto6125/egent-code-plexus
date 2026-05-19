//! Top-level Go `const X = ...` coverage.
//!
//! Regression: Go `queries.scm` had no const_declaration query and the
//! GoSpec map had no `const.name` entry, so all top-level `const X =
//! "..."` and `const ( A = 1; B = 2 )` declarations silently fell
//! through to the parser. 42 ref_over Const rows on `.sample_repo`
//! (gin auth.go `AuthUserKey/AuthProxyUserKey`, codec/json/*.go
//! `Package`, binding/binding.go MIME constants).
//!
//! Captured at source_file scope so function-body const blocks are
//! intentionally skipped, mirroring the var-vs-local design (see
//! `drop-locals-is-design` project memory).

use cgn_analyzer::go::parser::GoProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = GoProvider::new().expect("provider");
    p.parse_file(Path::new("test.go"), src.as_bytes())
        .expect("parse")
}

#[test]
fn const_single_emits_const_kind() {
    let g = parse("package p\n\nconst AuthUserKey = \"user\"\n");
    let n = g
        .nodes
        .iter()
        .find(|n| n.name == "AuthUserKey")
        .expect("AuthUserKey missing");
    assert_eq!(n.kind, NodeKind::Const, "got {n:?}");
}

#[test]
fn const_block_emits_each_const() {
    let g = parse(
        "package p\n\nconst (\n    MIMEJSON = \"application/json\"\n    MIMEXML = \"application/xml\"\n)\n",
    );
    let a = g
        .nodes
        .iter()
        .find(|n| n.name == "MIMEJSON")
        .expect("MIMEJSON missing");
    let b = g
        .nodes
        .iter()
        .find(|n| n.name == "MIMEXML")
        .expect("MIMEXML missing");
    assert_eq!(a.kind, NodeKind::Const);
    assert_eq!(b.kind, NodeKind::Const);
}

#[test]
fn const_typed_emits_const_kind() {
    let g = parse("package p\n\nconst defaultMemory int64 = 32 << 20\n");
    let n = g
        .nodes
        .iter()
        .find(|n| n.name == "defaultMemory")
        .expect("defaultMemory missing");
    assert_eq!(n.kind, NodeKind::Const);
}

#[test]
fn function_body_const_is_dropped() {
    // Source-file anchor in queries.scm means body-local `const` blocks
    // (legal Go, rare in practice) are intentionally not captured. This
    // matches the var/local split — locals stay out of the LLM index.
    let g = parse("package p\n\nfunc f() {\n    const local = 1\n    _ = local\n}\n");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.name == "local" && n.kind == NodeKind::Const),
        "function-body `const local` must not surface as a Const node"
    );
}
