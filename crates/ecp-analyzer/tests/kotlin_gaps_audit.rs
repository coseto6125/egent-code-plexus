#[test]
fn type_alias_emits_typedef() {
    // WHY: type aliases are real reference targets — `ecp find Callback` must
    // resolve without grep, and impact queries need the Typedef→callee edges.
    // Kotlin `typealias` is filter-(A) coverage: the graph was missing it,
    // causing agents to fall back to BM25/grep for alias lookups.
    use ecp_analyzer::kotlin::parser::KotlinProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use ecp_core::graph::NodeKind;
    use std::path::Path;

    let p = KotlinProvider::new().expect("provider");
    let source = "typealias Callback = (String) -> Unit\n";
    let g = p
        .parse_file(Path::new("Test.kt"), source.as_bytes())
        .expect("parse");

    let typedef_node = g
        .nodes
        .iter()
        .find(|n| n.name == "Callback" && n.kind == NodeKind::Typedef);
    assert!(
        typedef_node.is_some(),
        "Expected a Typedef node named 'Callback', got: {:?}",
        g.nodes
            .iter()
            .map(|n| (&n.name, n.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn companion_object_method_is_method_kind() {
    // WHY: companion object methods are class-level (static-equivalent) members;
    // emitting them as Function instead of Method causes impact queries to miss
    // class-level dispatch and agents to misclassify the method's scope.
    use ecp_analyzer::kotlin::parser::KotlinProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use ecp_core::graph::NodeKind;
    use std::path::Path;

    let p = KotlinProvider::new().expect("provider");
    let source = r#"
class Foo {
    companion object {
        fun bar() {}
    }
}
"#;
    let g = p
        .parse_file(Path::new("Test.kt"), source.as_bytes())
        .expect("parse");

    let foo_node = g
        .nodes
        .iter()
        .find(|n| n.name == "Foo" && n.kind == NodeKind::Class);
    assert!(
        foo_node.is_some(),
        "Expected a Class node named 'Foo', got: {:?}",
        g.nodes
            .iter()
            .map(|n| (&n.name, n.kind))
            .collect::<Vec<_>>()
    );

    let bar_node = g.nodes.iter().find(|n| n.name == "bar");
    assert!(
        bar_node.is_some(),
        "Expected a node named 'bar', got: {:?}",
        g.nodes
            .iter()
            .map(|n| (&n.name, n.kind))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        bar_node.unwrap().kind,
        NodeKind::Method,
        "companion object method 'bar' should be Method, not {:?}",
        bar_node.unwrap().kind
    );
}
