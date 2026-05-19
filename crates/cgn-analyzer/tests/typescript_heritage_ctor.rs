use graph_nexus_analyzer::typescript::TypeScriptProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = TypeScriptProvider::new().expect("provider");
    p.parse_file(Path::new("test.ts"), src.as_bytes())
        .expect("parse")
}

// ── Heritage: extends ─────────────────────────────────────────────────────────

#[test]
fn class_extends_populates_heritage() {
    let g = parse("class Cat extends Animal {}");
    let cat = g.nodes.iter().find(|n| n.name == "Cat").expect("Cat node");
    assert_eq!(cat.heritage, vec!["Animal"], "nodes: {:?}", g.nodes);
}

#[test]
fn exported_class_extends_populates_heritage() {
    let g = parse("export class Cat extends Animal {}");
    let cat = g.nodes.iter().find(|n| n.name == "Cat").expect("Cat node");
    assert_eq!(cat.heritage, vec!["Animal"], "nodes: {:?}", g.nodes);
    assert!(cat.is_exported);
}

// ── Heritage: implements ──────────────────────────────────────────────────────

#[test]
fn class_implements_populates_heritage() {
    let g = parse("class Dog implements Animal {}");
    let dog = g.nodes.iter().find(|n| n.name == "Dog").expect("Dog node");
    assert_eq!(dog.heritage, vec!["Animal"], "nodes: {:?}", g.nodes);
}

#[test]
fn class_implements_multiple_captures_all() {
    let g = parse("class Foo implements IBar, IBaz {}");
    let foo = g.nodes.iter().find(|n| n.name == "Foo").expect("Foo node");
    let mut heritage = foo.heritage.clone();
    heritage.sort();
    assert_eq!(heritage, vec!["IBar", "IBaz"], "nodes: {:?}", g.nodes);
}

#[test]
fn class_extends_and_implements_captures_all() {
    let g = parse("class Cat extends Animal implements IFoo {}");
    let cat = g.nodes.iter().find(|n| n.name == "Cat").expect("Cat node");
    let mut heritage = cat.heritage.clone();
    heritage.sort();
    assert_eq!(heritage, vec!["Animal", "IFoo"], "nodes: {:?}", g.nodes);
}

// ── Heritage: interface extends ───────────────────────────────────────────────

#[test]
fn interface_extends_populates_heritage() {
    let g = parse("interface IFoo extends IBar {}");
    let iface = g
        .nodes
        .iter()
        .find(|n| n.name == "IFoo")
        .expect("IFoo node");
    assert_eq!(iface.heritage, vec!["IBar"], "nodes: {:?}", g.nodes);
}

#[test]
fn interface_extends_multiple_captures_all() {
    let g = parse("interface IFoo extends IBar, IBaz {}");
    let iface = g
        .nodes
        .iter()
        .find(|n| n.name == "IFoo")
        .expect("IFoo node");
    let mut heritage = iface.heritage.clone();
    heritage.sort();
    assert_eq!(heritage, vec!["IBar", "IBaz"], "nodes: {:?}", g.nodes);
}

// ── Constructor ───────────────────────────────────────────────────────────────

#[test]
fn constructor_emits_constructor_kind() {
    let g = parse("class Foo { constructor(public x: string) {} }");
    let ctors: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Constructor)
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(ctors, vec!["constructor"], "nodes: {:?}", g.nodes);
}

#[test]
fn constructor_not_double_counted_as_method() {
    let g = parse("class Foo { constructor() {} }");
    let methods: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method && n.name == "constructor")
        .map(|n| n.name.as_str())
        .collect();
    assert!(
        methods.is_empty(),
        "constructor should not also emit as Method: {:?}",
        g.nodes
    );
}

#[test]
fn non_constructor_methods_still_emit_as_method() {
    let g = parse("class Foo { greet(): void {} }");
    let methods: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method)
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(methods, vec!["greet"], "nodes: {:?}", g.nodes);
}

// ── Combined fixture ──────────────────────────────────────────────────────────

#[test]
fn full_class_hierarchy_fixture() {
    let src = r#"
interface Animal { name: string; }
class Cat extends Animal {
  constructor(public name: string) {}
}
class Dog implements Animal {
  constructor(public name: string) {}
}
"#;
    let g = parse(src);

    let cat = g.nodes.iter().find(|n| n.name == "Cat").expect("Cat");
    assert_eq!(cat.heritage, vec!["Animal"]);

    let dog = g.nodes.iter().find(|n| n.name == "Dog").expect("Dog");
    assert_eq!(dog.heritage, vec!["Animal"]);

    let ctor_count = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Constructor)
        .count();
    assert_eq!(
        ctor_count, 2,
        "expected 2 Constructor nodes, nodes: {:?}",
        g.nodes
    );
}
