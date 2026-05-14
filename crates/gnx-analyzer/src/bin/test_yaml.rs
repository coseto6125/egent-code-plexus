use tree_sitter::Parser;

fn main() {
    let mut parser = Parser::new();
    let language = tree_sitter_yaml::LANGUAGE.into();
    parser.set_language(&language).unwrap();
    let source = "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n";
    let tree = parser.parse(source, None).unwrap();
    println!("{}", tree.root_node().to_sexp());
}
