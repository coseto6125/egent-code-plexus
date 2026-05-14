use tree_sitter::Parser;

fn main() {
    let code = "
#include <iostream>
namespace ns = std;
class A {};
class B : public A {};
int main() { return 0; }
";
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(code, None).unwrap();
    println!("{}", tree.root_node().to_sexp());
}
