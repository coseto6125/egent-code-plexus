use tree_sitter::Parser;

fn main() {
    let code = "public class A : B, C { public int f() { return 1; } }";
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(code, None).unwrap();
    println!("{}", tree.root_node().to_sexp());
}
