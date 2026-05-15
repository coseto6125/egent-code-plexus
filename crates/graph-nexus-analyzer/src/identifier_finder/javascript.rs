//! JavaScript identifier finder. Covers:
//! - `identifier` (functions, variables, parameters)
//! - `property_identifier` (member access, object literal keys)
//! - `shorthand_property_identifier` (`{ foo }` shorthand)

use super::generic::find_by_kinds;
use graph_nexus_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &[
    "identifier",
    "property_identifier",
    "shorthand_property_identifier",
    "shorthand_property_identifier_pattern",
];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_javascript::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_function_and_calls() {
        let src = b"function foo() { return foo(); }\nfoo();\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn finds_member_access() {
        let src = b"obj.foo = 1;\nobj.foo();\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 2, "{:?}", hits);
    }

    #[test]
    fn ignores_string_literals() {
        let src = b"const x = 'foo';\nlet foo = 1;\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 1, "{:?}", hits);
    }
}
