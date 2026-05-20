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
fn test_inline_mod_emits_module() {
    let src = "pub mod utils { pub fn helper() {} }";
    let nodes = parse(src);
    let m = nodes
        .iter()
        .find(|(n, _)| n == "utils")
        .expect("utils not found");
    assert_eq!(m.1, NodeKind::Module, "inline mod must be NodeKind::Module");
}

#[test]
fn test_mod_declaration_emits_module() {
    let src = "pub mod config;";
    let nodes = parse(src);
    let m = nodes
        .iter()
        .find(|(n, _)| n == "config")
        .expect("config not found");
    assert_eq!(m.1, NodeKind::Module);
}

#[test]
fn test_private_mod_emits_module() {
    let src = "mod internal { fn secret() {} }";
    let nodes = parse(src);
    let m = nodes
        .iter()
        .find(|(n, _)| n == "internal")
        .expect("internal not found");
    assert_eq!(m.1, NodeKind::Module);
}

#[test]
fn test_multiple_mods_all_captured() {
    let src = r#"
pub mod http;
pub mod db;
mod internal;
"#;
    let nodes = parse(src);
    let mod_names: Vec<&str> = nodes
        .iter()
        .filter(|(_, k)| *k == NodeKind::Module)
        .map(|(n, _)| n.as_str())
        .collect();
    assert!(mod_names.contains(&"http"), "http missing: {mod_names:?}");
    assert!(mod_names.contains(&"db"), "db missing: {mod_names:?}");
    assert!(
        mod_names.contains(&"internal"),
        "internal missing: {mod_names:?}"
    );
}
