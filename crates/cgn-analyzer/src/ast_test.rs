use tree_sitter::Parser;

#[test]
fn test_dart_ast() {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_dart::LANGUAGE.into()).unwrap();
    let source = r#"
import 'package:foo/foo.dart' as foo;

class MyClass extends BaseClass implements MyInterface {
  int myMethod() { return 1; }
}

void myFunction() {}
"#;
    let tree = parser.parse(source, None).unwrap();
    println!("DART AST: {}", tree.root_node().to_sexp());
}

#[test]
fn test_swift_ast() {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_swift::LANGUAGE.into()).unwrap();
    let source = r#"
public class MyClass: BaseClass, MyProtocol {
    public func myFunc() -> Int {
        return 1
    }
}
import Foundation
"#;
    let tree = parser.parse(source, None).unwrap();
    println!("SWIFT AST: {}", tree.root_node().to_sexp());
}
