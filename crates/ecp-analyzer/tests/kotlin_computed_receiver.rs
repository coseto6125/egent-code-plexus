//! A computed receiver (`Other().m()`, `other().m()`) is no `this`: its
//! call never binds the caller's own `m` by name, even when `Other.m` is
//! declared first. A constructor call of a class this file declares types
//! the receiver; any other computed receiver whose `m` the caller's type
//! declares is an untyped member call. Elsewhere the bare name still
//! reaches a Kotlin extension function, which is free.

use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::CallSite;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use std::path::Path;

const SOURCE: &str = r#"class Other {
    fun m() {}
}
fun Other.ext() {}
class C {
    fun m() {}
    fun other(): Other = Other()
    fun viaConstructor() {
        Other().m()
    }
    fun viaCall() {
        other().m()
    }
    fun viaExtension() {
        other().ext()
    }
}
"#;

fn build() -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(
        KotlinProvider::new()
            .unwrap()
            .parse_file(Path::new("C.kt"), SOURCE.as_bytes())
            .unwrap(),
    );
    builder.build()
}

fn raw_calls(caller: &str) -> Vec<String> {
    let local = KotlinProvider::new()
        .unwrap()
        .parse_file(Path::new("C.kt"), SOURCE.as_bytes())
        .unwrap();
    let node = local.nodes.iter().find(|n| n.name == caller).unwrap();
    node.calls.clone()
}

/// Owner classes of the `callee` nodes that `caller` calls, sorted.
fn owners_called(graph: &ZeroCopyGraph, caller: &str, callee: &str) -> Vec<String> {
    let pool = &graph.string_pool;
    let name = |i: u32| graph.nodes[i as usize].name.resolve(pool);
    let source = (0..graph.nodes.len() as u32)
        .find(|&i| name(i) == caller)
        .unwrap();
    let mut owners: Vec<String> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls && e.source == source && name(e.target) == callee)
        .map(|e| {
            graph.nodes[e.target as usize]
                .owner_class
                .resolve(pool)
                .to_string()
        })
        .collect();
    owners.sort();
    owners
}

#[test]
fn test_kotlin_callee_constructor_receiver_emits_class_member() {
    assert!(raw_calls("viaConstructor").contains(&"Other.m".to_string()));
}

#[test]
fn test_kotlin_callee_computed_receiver_with_own_member_emits_untyped_member() {
    let calls = raw_calls("viaCall");
    assert!(calls.contains(&CallSite::untyped_member("m")), "{calls:?}");
    assert!(!calls.contains(&"m".to_string()), "{calls:?}");
}

#[test]
fn test_resolve_call_kotlin_constructor_receiver_declared_after_other_binds_other_member() {
    assert_eq!(owners_called(&build(), "viaConstructor", "m"), ["Other"]);
}

#[test]
fn test_resolve_call_kotlin_computed_receiver_binds_no_own_member() {
    let owners = owners_called(&build(), "viaCall", "m");
    assert!(!owners.contains(&"C".to_string()), "{owners:?}");
}

/// No enclosing type declares `ext`: the bare name keeps binding the
/// extension function.
#[test]
fn test_resolve_call_kotlin_computed_receiver_extension_binds_top_level_function() {
    assert_eq!(owners_called(&build(), "viaExtension", "ext"), [""]);
}
