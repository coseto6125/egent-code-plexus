use ecp_analyzer::java::JavaProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::python::PythonProvider;
use ecp_analyzer::ruby::RubyProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;
use std::time::Instant;

fn main() {
    let language = std::env::var("ECP_CHAIN_LANGUAGE").unwrap_or_else(|_| "js".into());
    let provider: Box<dyn LanguageProvider> = match language.as_str() {
        "js" => Box::new(JavaScriptProvider::new().unwrap()),
        "ts" => Box::new(TypeScriptProvider::new().unwrap()),
        "java" => Box::new(JavaProvider::new().unwrap()),
        "py" => Box::new(PythonProvider::new().unwrap()),
        "rb" => Box::new(RubyProvider::new().unwrap()),
        _ => panic!("unknown language"),
    };
    let literal = std::env::var("ECP_CHAIN_LITERAL").unwrap_or_else(|_| "x".into());
    for length in std::env::args().skip(1) {
        let length: usize = length.parse().unwrap();
        let chain = format!(".option(\"{literal}\")").repeat(length);
        let source = match language.as_str() {
            "js" | "ts" => format!("function caller(p) {{ p{chain}; }}"),
            "java" => format!("class App {{ void caller(Builder p) {{ p{chain}; }} }}"),
            "py" => format!("def caller(p):\n    p{chain}\n"),
            "rb" => format!(
                "def caller\n  Item{}.save\nend\n",
                ".where(:x)".repeat(length)
            ),
            _ => unreachable!(),
        };
        let started = Instant::now();
        let graph = provider
            .parse_file(Path::new(&format!("chain.{language}")), source.as_bytes())
            .unwrap();
        let caller = graph
            .nodes
            .iter()
            .find(|node| node.name == "caller")
            .unwrap();
        assert_eq!(caller.calls.len(), length + usize::from(language == "rb"));
        println!(
            "language={language} length={length} bytes={} parse_seconds={:.6} calls={}",
            source.len(),
            started.elapsed().as_secs_f64(),
            caller.calls.len()
        );
    }
}
