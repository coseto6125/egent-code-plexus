use gnx_analyzer::typescript::parser::TypeScriptProvider;
use gnx_core::analyzer::provider::LanguageProvider;

fn main() {
    let code = "export const NestFactory = new NestFactoryStatic();";
    let provider = TypeScriptProvider::new().unwrap();
    let graph = provider
        .parse_file(std::path::Path::new("test.ts"), code.as_bytes())
        .unwrap();
    println!("{:?}", graph.nodes);
}
