//! Solidity identifier finder.

use super::generic::find_by_kinds;
use cgn_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["identifier"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_solidity::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_function_name_and_call() {
        let src = b"contract C { function foo() public {} function bar() public { foo(); } }";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 2, "{:?}", hits);
    }
}
