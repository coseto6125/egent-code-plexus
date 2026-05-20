use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(source: &str) -> Vec<(String, NodeKind)> {
    let provider = RustProvider::new().expect("RustProvider::new");
    let graph = provider
        .parse_file(Path::new("test.rs"), source.as_bytes())
        .expect("parse_file");
    graph
        .nodes
        .iter()
        .map(|n| (n.name.clone(), n.kind))
        .collect()
}

#[test]
fn test_inherent_impl_emits_impl() {
    let src = r#"
struct Dog;
impl Dog {
    fn bark(&self) {}
}
"#;
    let nodes = parse(src);
    let imp = nodes
        .iter()
        .find(|(n, k)| n == "Dog" && *k == NodeKind::Impl);
    assert!(
        imp.is_some(),
        "impl Dog must produce NodeKind::Impl for 'Dog'; got: {nodes:?}"
    );
}

#[test]
fn test_trait_impl_emits_impl() {
    let src = r#"
trait Speak { fn speak(&self); }
struct Cat;
impl Speak for Cat {
    fn speak(&self) {}
}
"#;
    let nodes = parse(src);
    // The impl_item.name capture is the `type:` field — `Cat` in `impl Speak for Cat`
    let imp = nodes
        .iter()
        .find(|(n, k)| n == "Cat" && *k == NodeKind::Impl);
    assert!(
        imp.is_some(),
        "impl Speak for Cat must produce NodeKind::Impl 'Cat'; got: {nodes:?}"
    );
}

#[test]
fn test_impl_methods_still_emitted() {
    let src = r#"
struct Calc;
impl Calc {
    pub fn add(&self, a: i32, b: i32) -> i32 { a + b }
    pub fn sub(&self, a: i32, b: i32) -> i32 { a - b }
}
"#;
    let nodes = parse(src);
    let add = nodes.iter().find(|(n, _)| n == "add");
    assert!(
        add.is_some(),
        "impl methods must still be emitted: {nodes:?}"
    );
    let sub = nodes.iter().find(|(n, _)| n == "sub");
    assert!(
        sub.is_some(),
        "impl methods must still be emitted: {nodes:?}"
    );
}

#[test]
fn test_struct_and_impl_both_emitted() {
    let src = r#"
pub struct Server { port: u16 }
impl Server {
    pub fn new(port: u16) -> Self { Server { port } }
    pub fn start(&self) {}
}
"#;
    let nodes = parse(src);
    let strct = nodes
        .iter()
        .find(|(n, k)| n == "Server" && *k == NodeKind::Struct);
    assert!(
        strct.is_some(),
        "Server struct must be NodeKind::Struct: {nodes:?}"
    );
    let imp = nodes
        .iter()
        .find(|(n, k)| n == "Server" && *k == NodeKind::Impl);
    assert!(
        imp.is_some(),
        "Server impl must be NodeKind::Impl: {nodes:?}"
    );
}
