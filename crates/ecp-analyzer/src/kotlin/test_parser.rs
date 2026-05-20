use tree_sitter::{Parser, Query};
fn main() {
    let language = tree_sitter_kotlin::language();
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    let source = "import java.util.List\nclass MyClass { fun myFunc() {} }\ninterface MyInterface {}";
    let tree = parser.parse(source, None).unwrap();
    println!("{}", tree.root_node().to_sexp());
}
