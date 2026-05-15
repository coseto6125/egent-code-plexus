//! Kotlin identifier finder. Tree-sitter-kotlin uses `simple_identifier`
//! for almost every identifier-position node (classes, functions,
//! variables, properties).

use super::generic::find_by_kinds;
use graph_nexus_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["simple_identifier", "type_identifier"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_kotlin::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_function_and_calls() {
        let src = b"fun foo() {}\nfun main() { foo(); foo() }\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn finds_class_decl_and_use() {
        let src = b"class Foo\nfun make(): Foo = Foo()\n";
        let hits = find_identifier_occurrences(src, "Foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn ignores_string_literals() {
        let src = b"val s = \"foo\"\nval foo = 1\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 1, "{:?}", hits);
    }
}
