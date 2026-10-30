//! Kotlin parser fixes: Ktor framework refs name the function they sit in.

use ecp_analyzer::framework_helpers::MODULE_LEVEL_SOURCE;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    KotlinProvider::new()
        .expect("KotlinProvider::new")
        .parse_file(Path::new("a.kt"), src.as_bytes())
        .expect("parse_file")
}

const IMPORT: &str = "import io.ktor.server.routing.*\n\n";

/// Names of the Function/Method nodes whose span contains `span`.
fn enclosing_names(g: &LocalGraph, span: (u32, u32, u32, u32)) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .filter(|n| {
            (n.span.0, n.span.1) <= (span.0, span.1) && (n.span.2, n.span.3) >= (span.2, span.3)
        })
        .map(|n| n.name.as_str())
        .collect()
}

// FU-2026-10-30-baa83b70b9a0
#[test]
fn test_kotlin_ktor_ref_source_name_is_an_enclosing_function_not_module() {
    let g = parse(&format!(
        "{IMPORT}fun Application.module() {{\n    routing {{\n        get(\"/x\") {{ call.respondText(\"ok\") }}\n    }}\n}}\n"
    ));
    assert_eq!(g.framework_refs.len(), 1, "{:?}", g.framework_refs);
    let r = &g.framework_refs[0];
    assert_ne!(r.source_name, MODULE_LEVEL_SOURCE);
    let enclosing = enclosing_names(&g, r.span);
    assert!(
        enclosing.contains(&r.source_name.as_str()),
        "source {:?} not in enclosing {enclosing:?}",
        r.source_name
    );
}

/// A route's source is the named function it is declared in, never the
/// anonymous lambda of the `routing { }` block around it.
#[test]
fn test_kotlin_ktor_ref_source_name_is_the_named_function_not_a_lambda() {
    let g = parse(&format!(
        "{IMPORT}fun Application.module() {{\n    routing {{\n        get(\"/x\") {{ call.respondText(\"ok\") }}\n    }}\n}}\n"
    ));
    assert_eq!(g.framework_refs.len(), 1, "{:?}", g.framework_refs);
    assert_eq!(g.framework_refs[0].source_name, "module");
}

/// Nested route blocks (`route { get }`) still resolve to the named function.
#[test]
fn test_kotlin_ktor_ref_in_nested_route_blocks_names_the_function() {
    let g = parse(&format!(
        "{IMPORT}fun Application.module() {{\r\n    routing {{\r\n        route(\"/a\") {{\r\n            get(\"/x\") {{ call.respondText(\"ok\") }}\r\n        }}\r\n    }}\r\n}}\r\n"
    ));
    let r = g
        .framework_refs
        .iter()
        .find(|r| r.target_name == "/x")
        .expect("get ref");
    assert_eq!(r.source_name, "module");
}

/// A route with no named function around it (top-level property lambda)
/// keeps the module-level source.
#[test]
fn test_kotlin_ktor_ref_outside_any_named_function_is_module_level() {
    let g = parse(&format!(
        "{IMPORT}val r = routing {{\n    get(\"/x\") {{ call.respondText(\"ok\") }}\n}}\n"
    ));
    for r in g.framework_refs.iter().filter(|r| r.target_name == "/x") {
        assert_eq!(r.source_name, MODULE_LEVEL_SOURCE);
    }
}

// FU-2026-10-30-baa83b70b9a0
#[test]
fn test_kotlin_ktor_refs_in_two_functions_carry_their_own_source_names() {
    let g = parse(&format!(
        "{IMPORT}fun Application.a() {{\n    routing {{\n        get(\"/a\") {{ call.respondText(\"a\") }}\n    }}\n}}\n\nfun Application.b() {{\n    routing {{\n        post(\"/b\") {{ call.respondText(\"b\") }}\n    }}\n}}\n"
    ));
    assert_eq!(g.framework_refs.len(), 2, "{:?}", g.framework_refs);
    let (ra, rb) = (&g.framework_refs[0], &g.framework_refs[1]);
    assert_eq!(
        (ra.target_name.as_str(), rb.target_name.as_str()),
        ("/a", "/b")
    );
    assert_ne!(ra.source_name, rb.source_name);
    assert!(enclosing_names(&g, ra.span).contains(&ra.source_name.as_str()));
    assert!(enclosing_names(&g, rb.span).contains(&rb.source_name.as_str()));
}

// FU-2026-10-30-baa83b70b9a0 guard: the rest of the ref is unchanged.
#[test]
fn test_kotlin_ktor_ref_keeps_target_reason_and_span() {
    let g = parse(&format!(
        "{IMPORT}fun Application.module() {{\n    routing {{\n        get(\"/x\") {{ call.respondText(\"ok\") }}\n    }}\n}}\n"
    ));
    let r = &g.framework_refs[0];
    assert_eq!(
        (r.target_name.as_str(), r.reason.as_str(), r.span),
        ("/x", "ktor-route-get", (4, 8, 4, 44))
    );
}

// FU-2026-10-30-baa83b70b9a0 guard: `annotation class` still maps to Annotation.
#[test]
fn test_kotlin_annotation_class_with_other_modifiers_stays_annotation() {
    let g = parse("public annotation class Marker\nclass Plain\n");
    let kind = |name: &str| g.nodes.iter().find(|n| n.name == name).map(|n| n.kind);
    assert_eq!(kind("Marker"), Some(NodeKind::Annotation));
    assert_eq!(kind("Plain"), Some(NodeKind::Class));
}
