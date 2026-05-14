use std::fs;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

fn main() {
    let file = std::path::Path::new(".sample_repo/Swift/Source/Core/Session.swift");
    let source = fs::read(file).unwrap();
    let mut parser = Parser::new();
    let language = tree_sitter_swift::LANGUAGE.into();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(&source, None).unwrap();

    let query_source = "
(class_declaration
  name: (type_identifier) @name.class
) @class
    ";
    let query = Query::new(&language, query_source).unwrap();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_slice());

    while let Some(m) = matches.next() {
        for cap in m.captures {
            if cap.index == query.capture_index_for_name("name.class").unwrap() {
                println!(
                    "Matched class name: {:?}",
                    std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                        .unwrap()
                );
            }
        }
    }
}
