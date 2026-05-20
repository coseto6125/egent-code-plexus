use tree_sitter::Parser;

fn main() {
    let code = "class A : B, C { fun f() {} }";
    let mut parser = Parser::new();
    // Use the unsafe transmute for kotlin again just for this test if needed
    // or use the git version if it's already in the workspace
    parser
        .set_language(&tree_sitter_kotlin::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(code, None).unwrap();
    println!("{}", tree.root_node().to_sexp());
}
