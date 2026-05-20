use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ignore::WalkBuilder;
use std::fs;

fn main() {
    let mut files = Vec::new();
    let repo_path = std::path::PathBuf::from(".sample_repo/JavaScript");
    for entry in WalkBuilder::new(&repo_path).build().flatten() {
        if entry.path().is_file() && entry.path().extension().and_then(|s| s.to_str()) == Some("js")
        {
            files.push(entry.path().to_path_buf());
        }
    }

    let provider = JavaScriptProvider::new().unwrap();
    for file in files {
        println!("Parsing: {:?}", file);
        let source = fs::read(&file).unwrap();
        let _ = provider.parse_file(&file, &source);
    }
}
