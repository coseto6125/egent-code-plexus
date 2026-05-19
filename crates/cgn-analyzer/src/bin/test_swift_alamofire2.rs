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
    for i in 0..root.child_count() {
        let child = root.child(i).unwrap();
        let text =
            &source[child.start_byte()..std::cmp::min(child.end_byte(), child.start_byte() + 50)];
        println!(
            "Child {}: {} -> {:?}",
            i,
            child.kind(),
            std::str::from_utf8(text).unwrap_or("")
        );
    }
}
