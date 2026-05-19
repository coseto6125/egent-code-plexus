//! Swift identifier finder. Tree-sitter-swift uses `simple_identifier`
//! for most identifier-position nodes; `type_identifier` is used in
//! grammar paths but most type names also appear under `simple_identifier`
//! in practice.

use super::generic::find_by_kinds;
use cgn_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["simple_identifier", "type_identifier"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_swift::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_function_and_calls() {
        let src = b"func foo() {}\nfoo()\nfoo()\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn finds_class_decl_and_use() {
        let src = b"class Foo {}\nlet x = Foo()\n";
        let hits = find_identifier_occurrences(src, "Foo");
        assert_eq!(hits.len(), 2, "{:?}", hits);
    }

    #[test]
    fn ignores_string_literals() {
        let src = b"let s = \"foo\"\nlet foo = 1\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 1, "{:?}", hits);
    }
}
