use tree_sitter::{Parser, Query, QueryCursor};
use streaming_iterator::StreamingIterator;

fn main() {
    let language: tree_sitter::Language = tree_sitter_vyper::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    
    let source = br#"
import ERC20Interface

DECIMALS: constant(uint256) = 18
MAX_SUPPLY: constant(uint256) = 1000000
totalSupply: uint256 = 0

@external
def transfer(_to: address, _value: uint256) -> bool:
    return True

@internal
def _mint(_to: address, _value: uint256):
    self.totalSupply += _value
"#;
    
    let tree = parser.parse(source, None).unwrap();
    println!("=== CST ===");
    println!("{}", tree.root_node().to_sexp());
    
    println!("\n=== Query test ===");
    let query_source = r#"
(function_definition
  (identifier) @function.name) @function
(variable_definition
  (identifier) @const.name) @const
(constant_definition
  (identifier) @const.name) @const
(import_statement
  (identifier) @import.source) @import
"#;

    let query = Query::new(&language, query_source).unwrap();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_slice());

    let idx_fn_name = query.capture_index_for_name("function.name");
    let idx_const_name = query.capture_index_for_name("const.name");
    let idx_import = query.capture_index_for_name("import.source");

    while let Some(m) = matches.next() {
        for cap in m.captures {
            let text = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]).unwrap();
            if Some(cap.index) == idx_fn_name {
                println!("FUNCTION: {text}");
            } else if Some(cap.index) == idx_const_name {
                println!("CONST: {text}");
            } else if Some(cap.index) == idx_import {
                println!("IMPORT: {text}");
            }
        }
    }
}
