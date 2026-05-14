use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

fn main() {
    let code = "open class Session: @unchecked Sendable {}";
    let mut parser = Parser::new();
    let language = tree_sitter_swift::LANGUAGE.into();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(code, None).unwrap();

    let query_source = "
(class_declaration
  name: (type_identifier) @name.class
  (inheritance_specifier inherits_from: (user_type (type_identifier) @heritage))?
) @class
    ";
    let query = Query::new(&language, query_source).unwrap();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), code.as_bytes());

    let mut count = 0;
    while matches.next().is_some() {
        count += 1;
    }
    println!("Matches found: {}", count);
}
