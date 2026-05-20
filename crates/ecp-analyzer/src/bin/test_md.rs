use tree_sitter::Parser;

fn main() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_md::LANGUAGE.into())
        .unwrap();
    let tree = parser
        .parse("# Heading 1\nSome text\n## Heading 2\nMore text", None)
        .unwrap();
    println!("{}", tree.root_node().to_sexp());
}
