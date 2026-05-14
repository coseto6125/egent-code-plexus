use tree_sitter::Parser;

fn main() {
    let code = "import a.b.C as D";
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_kotlin::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(code, None).unwrap();
    println!("{}", tree.root_node().to_sexp());
}
