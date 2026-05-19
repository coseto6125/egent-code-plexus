//! Dart identifier finder. Tree-sitter-dart uses `identifier` for most
//! identifier-position nodes (classes, functions, variables, parameters,
//! type references).

use super::generic::find_by_kinds;
use cgn_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["identifier", "type_identifier"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_dart::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_function_and_calls() {
        let src = b"void foo() {}\nvoid main() { foo(); foo(); }\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn finds_class_decl_and_use() {
        let src = b"class Foo {}\nFoo make() => Foo();\n";
        let hits = find_identifier_occurrences(src, "Foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn ignores_string_literals() {
        let src = b"var s = \"foo\";\nvar foo = 1;\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 1, "{:?}", hits);
    }
}
