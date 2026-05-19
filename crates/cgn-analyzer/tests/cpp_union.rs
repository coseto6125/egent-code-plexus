//! Cpp union_specifier coverage.
//!
//! Regression: prior to the dedicated `union_specifier` query in
//! `cpp/queries.scm`, `union X { ... }` and `typedef union X { ... } X;`
//! fell through to the typedef path (or were silently dropped),
//! producing 13 unpaired ref_over rows on `.sample_repo` (lua/lobject.h
//! GCObject/TString/Udata/Closure/TKey, lstate.h, luaconf.h luai_Cast,
//! nlohmann/json.hpp json_value, jemalloc emap_batch_lookup_result_u,
//! redis cluster_legacy.h clusterMsgData).
//!
//! Emit policy mirrors C: no dedicated NodeKind::Union — captured as
//! NodeKind::Struct (parity aggregator's struct-family EQUIV class
//! pairs gnx Struct ↔ ref Union).

use cgn_analyzer::cpp::parser::CppProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::RawNode;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = CppProvider::new().expect("CppProvider init");
    let graph = provider
        .parse_file(Path::new("t.cpp"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} `{name}` in {nodes:#?}"))
}

#[test]
fn plain_named_union_emits_struct() {
    let nodes = parse("union W128_T {\n    int s;\n    unsigned u[4];\n};\n");
    find(&nodes, "W128_T", NodeKind::Struct);
}

#[test]
fn typedef_union_same_name_emits_struct_and_typedef() {
    // `typedef union GCObject GCObject;` — the inner union_specifier
    // matches the new union query (Struct), and the outer type_definition
    // matches the existing typedef query (Typedef). Both must surface so
    // either-kind cypher lookups resolve the source declaration.
    let src = "typedef union GCObject GCObject;\n";
    let nodes = parse(src);
    find(&nodes, "GCObject", NodeKind::Struct);
    find(&nodes, "GCObject", NodeKind::Typedef);
}

#[test]
fn typedef_union_with_body_emits_struct() {
    let src = "\
typedef union TKey {\n\
    int gc;\n\
    char tt;\n\
} TKey;\n";
    let nodes = parse(src);
    find(&nodes, "TKey", NodeKind::Struct);
    find(&nodes, "TKey", NodeKind::Typedef);
}

#[test]
fn anonymous_union_typedef_emits_only_typedef() {
    // `typedef union { ... } Alias;` — no tag, so the union_specifier
    // query (which requires a `name: (type_identifier)` child) doesn't
    // match. Only the typedef alias surfaces, which is the correct
    // representation: there's no nameable union tag to emit.
    let src = "\
typedef union {\n\
    int gc;\n\
    char tt;\n\
} AnonAlias;\n";
    let nodes = parse(src);
    find(&nodes, "AnonAlias", NodeKind::Typedef);
    assert!(
        !nodes
            .iter()
            .any(|n| n.name == "AnonAlias" && n.kind == NodeKind::Struct),
        "anonymous-union typedef must not synthesize a Struct for the alias name"
    );
}
