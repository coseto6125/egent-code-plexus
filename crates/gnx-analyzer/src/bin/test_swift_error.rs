use std::fs;
use tree_sitter::Parser;

fn main() {
    let file = std::path::Path::new(".sample_repo/Swift/Source/Core/Session.swift");
    let source = fs::read(file).unwrap();
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(&source, None).unwrap();

    let root = tree.root_node();
    println!("{}", root.child(27).unwrap().to_sexp());
}
