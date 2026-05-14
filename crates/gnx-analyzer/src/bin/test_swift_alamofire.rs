use gnx_analyzer::swift::parser::SwiftProvider;
use gnx_core::analyzer::provider::LanguageProvider;
use std::fs;

fn main() {
    let provider = SwiftProvider::new().unwrap();
    let file = std::path::Path::new(".sample_repo/Swift/Source/Core/Session.swift");
    let source = fs::read(file).unwrap();
    let graph = provider.parse_file(file, &source).unwrap();
    println!("Total nodes: {}", graph.nodes.len());
    for node in graph.nodes {
        println!("{:?}", node);
    }
}
