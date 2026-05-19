use cgn_analyzer::c_sharp::parser::CSharpProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = CSharpProvider::new().expect("provider");
    p.parse_file(Path::new("test.cs"), src.as_bytes()).expect("parse")
}

fn namespaces(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Namespace)
        .map(|n| n.name.as_str())
        .collect()
}

fn classes(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Class)
        .map(|n| n.name.as_str())
        .collect()
}

#[test]
fn simple_namespace_and_class() {
    let src = r#"namespace Foo { class C {} }"#;
    let g = parse(src);
    let ns = namespaces(&g);
    assert_eq!(ns.len(), 1, "expected 1 Namespace, got {ns:?}");
    assert_eq!(ns[0], "Foo");
    let cls = classes(&g);
    assert_eq!(cls.len(), 1, "expected 1 Class, got {cls:?}");
    assert_eq!(cls[0], "C");
}

#[test]
fn dotted_namespace() {
    let src = r#"namespace Foo.Bar { class C {} }"#;
    let g = parse(src);
    let ns = namespaces(&g);
    assert_eq!(ns.len(), 1, "expected 1 Namespace, got {ns:?}");
    assert_eq!(ns[0], "Foo.Bar");
}

#[test]
fn file_scoped_namespace() {
    // File-scoped namespace (C# 10+): `namespace Foo;`
    let src = "namespace Foo;\nclass C {}";
    let g = parse(src);
    let ns = namespaces(&g);
    assert_eq!(ns.len(), 1, "expected 1 Namespace, got {ns:?}");
    assert_eq!(ns[0], "Foo");
}

#[test]
fn nested_namespace_emits_both() {
    let src = r#"
        namespace Outer {
            namespace Inner {
                class C {}
            }
        }
    "#;
    let g = parse(src);
    let mut ns = namespaces(&g);
    ns.sort_unstable();
    assert_eq!(ns.len(), 2, "expected 2 Namespaces, got {ns:?}");
    assert!(ns.contains(&"Outer"), "{ns:?}");
    assert!(ns.contains(&"Inner"), "{ns:?}");
}
