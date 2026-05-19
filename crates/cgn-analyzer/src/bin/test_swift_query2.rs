use graph_nexus_analyzer::swift::parser::SwiftProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;

fn main() {
    let code = "open class Session: @unchecked Sendable {}";
    let provider = SwiftProvider::new().unwrap();
    let graph = provider
        .parse_file(std::path::Path::new("test.swift"), code.as_bytes())
        .unwrap();
    println!("{:?}", graph.nodes);
}
