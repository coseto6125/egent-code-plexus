use cgn_analyzer::rust::parser::RustProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(source: &str) -> Vec<(String, NodeKind)> {
    let provider = RustProvider::new().expect("RustProvider::new");
    let graph = provider
        .parse_file(Path::new("test.rs"), source.as_bytes())
        .expect("parse_file");
    graph.nodes.iter().map(|n| (n.name.clone(), n.kind)).collect()
}

#[test]
fn test_type_alias_emits_typedef() {
    let src = "pub type Meters = f64;";
    let nodes = parse(src);
    let t = nodes.iter().find(|(n, _)| n == "Meters").expect("Meters not found");
    assert_eq!(t.1, NodeKind::Typedef, "type alias must be NodeKind::Typedef");
}

#[test]
fn test_private_type_alias_emits_typedef() {
    let src = "type Result<T> = std::result::Result<T, String>;";
    let nodes = parse(src);
    let t = nodes.iter().find(|(n, _)| n == "Result").expect("Result not found");
    assert_eq!(t.1, NodeKind::Typedef);
}

#[test]
fn test_const_emits_const() {
    let src = "pub const MAX_SIZE: usize = 1024;";
    let nodes = parse(src);
    let c = nodes.iter().find(|(n, _)| n == "MAX_SIZE").expect("MAX_SIZE not found");
    assert_eq!(c.1, NodeKind::Const, "const item must be NodeKind::Const");
}

#[test]
fn test_private_const_emits_const() {
    let src = "const BUFFER: u32 = 64;";
    let nodes = parse(src);
    let c = nodes.iter().find(|(n, _)| n == "BUFFER").expect("BUFFER not found");
    assert_eq!(c.1, NodeKind::Const);
}

#[test]
fn test_multiple_type_aliases() {
    let src = r#"
pub type Tx = tokio::sync::mpsc::Sender<String>;
pub type Rx = tokio::sync::mpsc::Receiver<String>;
const CAPACITY: usize = 32;
"#;
    let nodes = parse(src);
    let tx = nodes.iter().find(|(n, _)| n == "Tx").expect("Tx");
    assert_eq!(tx.1, NodeKind::Typedef);
    let rx = nodes.iter().find(|(n, _)| n == "Rx").expect("Rx");
    assert_eq!(rx.1, NodeKind::Typedef);
    let cap = nodes.iter().find(|(n, _)| n == "CAPACITY").expect("CAPACITY");
    assert_eq!(cap.1, NodeKind::Const);
}
