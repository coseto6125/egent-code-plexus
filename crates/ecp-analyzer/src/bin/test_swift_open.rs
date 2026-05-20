use tree_sitter::Parser;

fn main() {
    let code = "open class Session: @unchecked Sendable {}";
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(code, None).unwrap();
    let root = tree.root_node();
    println!("{}", root.to_sexp());

    // Print children
    let class_decl = root.child(0).unwrap();
    for i in 0..class_decl.child_count() {
        let child = class_decl.child(i as u32).unwrap();
        let text = &code[child.start_byte()..child.end_byte()];
        println!("Child {}: {} -> {}", i, child.kind(), text);
    }
}
