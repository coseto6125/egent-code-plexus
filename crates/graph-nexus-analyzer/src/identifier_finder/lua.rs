//! Lua identifier finder.

use super::generic::find_by_kinds;
use graph_nexus_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["identifier"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(source, target_name, &tree_sitter_lua::LANGUAGE.into(), KINDS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_function_def_and_call() {
        let src = b"function foo() end\nfoo()\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 2, "{:?}", hits);
    }

    #[test]
    fn ignores_string_literal() {
        let src = b"local x = \"foo\"\nfoo = 1\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 1, "{:?}", hits);
    }
}
