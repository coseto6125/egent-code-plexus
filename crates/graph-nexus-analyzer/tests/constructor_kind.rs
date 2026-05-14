//! Verify that each language with explicit constructor syntax emits a
//! `NodeKind::Constructor` node. Before this wiring the variant existed
//! but no parser ever produced it — see commit history for context.

use graph_nexus_analyzer::c_sharp::parser::CSharpProvider;
use graph_nexus_analyzer::java::parser::JavaProvider;
use graph_nexus_analyzer::javascript::parser::JavaScriptProvider;
use graph_nexus_analyzer::python::parser::PythonProvider;
use graph_nexus_analyzer::typescript::parser::TypeScriptProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

#[test]
fn python_dunder_init_is_constructor() {
    let src = "class Foo:\n    def __init__(self, x):\n        self.x = x\n";
    let p = PythonProvider::new().unwrap();
    let graph = p.parse_file(Path::new("t.py"), src.as_bytes()).unwrap();
    let ctor = graph
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Constructor));
    assert!(
        ctor.is_some(),
        "expected Python __init__ as Constructor, got {:?}",
        graph
            .nodes
            .iter()
            .map(|n| (n.name.clone(), n.kind))
            .collect::<Vec<_>>(),
    );
    assert_eq!(ctor.unwrap().name, "__init__");
}

#[test]
fn python_top_level_init_is_not_constructor() {
    // `__init__` at module scope is a regular function, not a class ctor.
    let src = "def __init__(x):\n    return x\n";
    let p = PythonProvider::new().unwrap();
    let graph = p.parse_file(Path::new("t.py"), src.as_bytes()).unwrap();
    assert!(
        graph
            .nodes
            .iter()
            .all(|n| !matches!(n.kind, NodeKind::Constructor)),
        "module-level __init__ should not be classified as Constructor",
    );
}

#[test]
fn typescript_constructor_method_is_constructor() {
    let src = "class Foo {\n  constructor(public x: number) {}\n  bar(): void {}\n}\n";
    let p = TypeScriptProvider::new().unwrap();
    let graph = p.parse_file(Path::new("t.ts"), src.as_bytes()).unwrap();
    let ctor = graph
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Constructor));
    assert!(
        ctor.is_some(),
        "expected constructor node, got {:?}",
        graph
            .nodes
            .iter()
            .map(|n| (n.name.clone(), n.kind))
            .collect::<Vec<_>>(),
    );
    assert_eq!(ctor.unwrap().name, "constructor");
    // Sibling `bar` must remain a plain Method.
    let bar = graph.nodes.iter().find(|n| n.name == "bar").unwrap();
    assert!(matches!(bar.kind, NodeKind::Method));
}

#[test]
fn javascript_constructor_method_is_constructor() {
    let src = "class Foo {\n  constructor(x) { this.x = x; }\n  bar() {}\n}\n";
    let p = JavaScriptProvider::new().unwrap();
    let graph = p.parse_file(Path::new("t.js"), src.as_bytes()).unwrap();
    let ctor = graph
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Constructor));
    assert!(
        ctor.is_some(),
        "expected JS constructor, got {:?}",
        graph
            .nodes
            .iter()
            .map(|n| (n.name.clone(), n.kind))
            .collect::<Vec<_>>(),
    );
    assert_eq!(ctor.unwrap().name, "constructor");
    let bar = graph.nodes.iter().find(|n| n.name == "bar").unwrap();
    assert!(matches!(bar.kind, NodeKind::Method));
}

#[test]
fn java_constructor_declaration_is_constructor() {
    let src = r#"
public class Foo {
    public Foo(int x) { this.x = x; }
    public void bar() {}
}
"#;
    let p = JavaProvider::new().unwrap();
    let graph = p.parse_file(Path::new("Foo.java"), src.as_bytes()).unwrap();
    let ctor = graph
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Constructor));
    assert!(
        ctor.is_some(),
        "expected Java constructor, got {:?}",
        graph
            .nodes
            .iter()
            .map(|n| (n.name.clone(), n.kind))
            .collect::<Vec<_>>(),
    );
    assert_eq!(ctor.unwrap().name, "Foo");
    let bar = graph.nodes.iter().find(|n| n.name == "bar").unwrap();
    assert!(matches!(bar.kind, NodeKind::Method));
}

#[test]
fn csharp_constructor_declaration_is_constructor() {
    let src = r#"
public class Foo {
    public Foo(int x) { this.x = x; }
    public void Bar() {}
}
"#;
    let p = CSharpProvider::new().unwrap();
    let graph = p.parse_file(Path::new("Foo.cs"), src.as_bytes()).unwrap();
    let ctor = graph
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Constructor));
    assert!(
        ctor.is_some(),
        "expected C# constructor, got {:?}",
        graph
            .nodes
            .iter()
            .map(|n| (n.name.clone(), n.kind))
            .collect::<Vec<_>>(),
    );
    assert_eq!(ctor.unwrap().name, "Foo");
    let bar = graph.nodes.iter().find(|n| n.name == "Bar").unwrap();
    assert!(matches!(bar.kind, NodeKind::Method));
}
