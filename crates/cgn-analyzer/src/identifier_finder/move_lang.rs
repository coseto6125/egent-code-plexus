//! Move identifier finder. tree-sitter-move uses kind-specific identifier
//! variants — covered all so rename catches every renameable surface.

use super::generic::find_by_kinds;
use graph_nexus_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &[
    "identifier",
    "constant_identifier",
    "function_identifier",
    "struct_identifier",
];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_move::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_function_def_and_call() {
        let src = b"module a::b { fun foo() {} fun bar() { foo(); } }";
        let hits = find_identifier_occurrences(src, "foo");
        assert!(hits.len() >= 2, "{:?}", hits);
    }
}
