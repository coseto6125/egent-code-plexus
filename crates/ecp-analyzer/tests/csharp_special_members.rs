//! Operator / event / indexer / destructor members must each emit a node so
//! that calls inside their bodies attach to an enclosing member instead of
//! being dropped by `attach_to_enclosing` (which gates on
//! Function | Method | Constructor). Before this change these members emitted
//! no node, silently corrupting `ecp impact`'s caller set for any function
//! called from inside them.

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = CSharpProvider::new().expect("provider");
    p.parse_file(Path::new("test.cs"), src.as_bytes())
        .expect("parse")
}

fn method_named<'a>(
    g: &'a LocalGraph,
    name: &str,
) -> Option<&'a ecp_core::analyzer::types::RawNode> {
    g.nodes
        .iter()
        .find(|n| n.kind == NodeKind::Method && n.name == name)
}

#[test]
fn operator_emits_method_node() {
    let g = parse(
        r#"
class Vec {
    public static Vec operator +(Vec a, Vec b) { Combine(a, b); return a; }
}
"#,
    );
    let op = method_named(&g, "op_+");
    assert!(
        op.is_some(),
        "expected op_+ Method node, nodes: {:?}",
        g.nodes
    );
    assert!(
        op.unwrap().calls.iter().any(|c| c == "Combine"),
        "body call Combine must attach to operator member, calls: {:?}",
        op.unwrap().calls
    );
}

#[test]
fn conversion_operator_emits_method_node() {
    let g = parse(
        r#"
class Money {
    public static explicit operator int(Money m) { return Round(m); }
}
"#,
    );
    let conv = method_named(&g, "op_Explicit");
    assert!(
        conv.is_some(),
        "expected op_Explicit Method node, nodes: {:?}",
        g.nodes
    );
    assert!(
        conv.unwrap().calls.iter().any(|c| c == "Round"),
        "body call Round must attach to conversion operator, calls: {:?}",
        conv.unwrap().calls
    );
}

#[test]
fn event_with_accessors_emits_node_and_attaches_body_call() {
    let g = parse(
        r#"
class Button {
    public event System.EventHandler Click { add { Register(value); } remove { } }
}
"#,
    );
    let ev = method_named(&g, "Click");
    assert!(
        ev.is_some(),
        "expected Click event Method node, nodes: {:?}",
        g.nodes
    );
    assert!(
        ev.unwrap().calls.iter().any(|c| c == "Register"),
        "body call Register must attach to event member, calls: {:?}",
        ev.unwrap().calls
    );
}

#[test]
fn event_field_emits_node_per_declarator() {
    // `event EventHandler A, B;` — both names must exist for who-subscribes queries.
    let g = parse(
        r#"
class Source {
    public event System.EventHandler Opened, Closed;
}
"#,
    );
    assert!(
        method_named(&g, "Opened").is_some(),
        "expected Opened event-field node, nodes: {:?}",
        g.nodes
    );
    assert!(
        method_named(&g, "Closed").is_some(),
        "expected Closed event-field node, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn indexer_emits_node_and_attaches_body_call() {
    let g = parse(
        r#"
class Grid {
    public int this[int i] { get { return Lookup(i); } }
}
"#,
    );
    let ix = method_named(&g, "this[...]");
    assert!(
        ix.is_some(),
        "expected this[...] indexer Method node, nodes: {:?}",
        g.nodes
    );
    assert!(
        ix.unwrap().calls.iter().any(|c| c == "Lookup"),
        "body call Lookup must attach to indexer member, calls: {:?}",
        ix.unwrap().calls
    );
}

#[test]
fn destructor_emits_node_and_attaches_body_call() {
    let g = parse(
        r#"
class Resource {
    ~Resource() { Release(); }
}
"#,
    );
    let dtor = method_named(&g, "~Resource");
    assert!(
        dtor.is_some(),
        "expected ~Resource destructor Method node, nodes: {:?}",
        g.nodes
    );
    assert!(
        dtor.unwrap().calls.iter().any(|c| c == "Release"),
        "body call Release must attach to destructor, calls: {:?}",
        dtor.unwrap().calls
    );
}
