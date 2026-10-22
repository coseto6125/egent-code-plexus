use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;
use std::time::Instant;

fn main() {
    let provider = JavaScriptProvider::new().unwrap();
    for length in std::env::args().skip(1) {
        let length: usize = length.parse().unwrap();
        let source = format!(
            "function caller(p) {{ p{}; }}",
            ".option('x')".repeat(length)
        );
        let started = Instant::now();
        let graph = provider
            .parse_file(Path::new("chain.js"), source.as_bytes())
            .unwrap();
        let caller = graph
            .nodes
            .iter()
            .find(|node| node.name == "caller")
            .unwrap();
        assert_eq!(caller.calls.len(), length);
        println!(
            "length={length} bytes={} parse_seconds={:.6} calls={}",
            source.len(),
            started.elapsed().as_secs_f64(),
            caller.calls.len()
        );
    }
}
