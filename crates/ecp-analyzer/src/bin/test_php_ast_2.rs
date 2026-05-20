use tree_sitter::Parser;

fn main() {
    let code = "<?php interface A extends B, C {}";
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
        .unwrap();
    let tree = parser.parse(code, None).unwrap();
    println!("{}", tree.root_node().to_sexp());
}
