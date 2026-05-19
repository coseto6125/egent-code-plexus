//! Java identifier finder. Covers:
//! - `identifier` (variables, methods, parameters, members)
//! - `type_identifier` (class / interface / enum names, type references)

use super::generic::find_by_kinds;
use cgn_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["identifier", "type_identifier"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_java::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_class_decl_and_use() {
        let src = b"class Foo {}\nclass Bar { Foo make() { return new Foo(); } }\n";
        let hits = find_identifier_occurrences(src, "Foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn finds_method_call() {
        let src = b"class C { void run() { foo(); foo(); } void foo() {} }\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn ignores_string_literals() {
        let src = b"class C { void m() { String s = \"foo\"; int foo = 1; } }\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 1, "{:?}", hits);
    }
}
