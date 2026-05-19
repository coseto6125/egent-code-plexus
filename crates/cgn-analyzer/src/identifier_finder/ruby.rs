//! Ruby identifier finder. Covers:
//! - `identifier` (local variables, parameters, method names)
//! - `constant` (Class, Module names — capitalized identifiers)
//!
//! Instance/class/global variables (`@foo`, `@@foo`, `$foo`) are not
//! handled here — the leading sigils complicate text comparison and the
//! rename is usually about the symbol family, not all sigil-prefixed
//! aliases.

use super::generic::find_by_kinds;
use cgn_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["identifier", "constant"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_ruby::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_method_and_calls() {
        let src = b"def foo\n  1\nend\nfoo\nfoo()\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn finds_class_decl_and_use() {
        let src = b"class Foo\nend\nFoo.new\n";
        let hits = find_identifier_occurrences(src, "Foo");
        assert_eq!(hits.len(), 2, "{:?}", hits);
    }

    #[test]
    fn ignores_string_literals() {
        let src = b"s = \"foo\"\nfoo = 1\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 1, "{:?}", hits);
    }
}
