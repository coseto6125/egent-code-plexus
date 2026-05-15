//! Find all `identifier` AST nodes in a Python source matching a target
//! name. Used by the rename Stage 2 pipeline to enumerate every byte-
//! range candidate that the rename must rewrite.
//!
//! Scope (P0): coarse — every `identifier` node whose `utf8_text` matches
//! the target. This catches function/class definitions, calls, attribute
//! accesses, import bindings, and (intentionally) any local variable that
//! happens to share the target's name. The dry-run diff is the user's
//! review surface to catch shadowing false-positives.

use graph_nexus_core::analyzer::types::IdentifierRange;
use tree_sitter::{Node, Parser};

/// Parse `source` as Python and return every `identifier` byte-range
/// whose text equals `target_name`. Returns an empty vec on parse
/// failure (defensive — the caller treats "no hits" as "skip file").
pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk(tree.root_node(), source, target_name, &mut out);
    out
}

fn walk(node: Node<'_>, source: &[u8], target: &str, out: &mut Vec<IdentifierRange>) {
    if node.kind() == "identifier" {
        if let Ok(text) = node.utf8_text(source) {
            if text == target {
                let start = node.start_position();
                out.push(IdentifierRange {
                    start_byte: node.start_byte(),
                    end_byte: node.end_byte(),
                    row: start.row,
                    col: start.column,
                });
            }
        }
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk(child, source, target, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_def_and_call_sites() {
        let src = b"def foo():\n    return foo\n\nfoo()\n";
        let hits = find_identifier_occurrences(src, "foo");
        // def name, return value, top-level call name = 3 hits
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn ignores_non_identifier_text() {
        // "foo" inside a string literal must NOT match.
        let src = b"x = \"foo\"\nfoo = 1\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(
            hits.len(),
            1,
            "string literal should not match; got {:?}",
            hits
        );
    }

    #[test]
    fn matches_attribute_member_identifier() {
        // `obj.foo` — the `foo` is an identifier under attribute, must match.
        let src = b"obj.foo()\nfoo = 2\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 2, "{:?}", hits);
    }
}
