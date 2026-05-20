use tree_sitter::Parser;

fn main() {
    let code = "class A extends B {}";
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(code, None).unwrap();
    println!("{}", tree.root_node().to_sexp());
}
