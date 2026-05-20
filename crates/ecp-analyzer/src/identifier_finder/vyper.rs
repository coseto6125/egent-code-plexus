//! Vyper identifier finder. tree-sitter-vyper grammar is Python-derived,
//! so `identifier` is the canonical kind.

use super::generic::find_by_kinds;
use ecp_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["identifier"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_vyper::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_def_and_call() {
        let src = b"@external\ndef foo() -> int128:\n    return 1\n@external\ndef bar() -> int128:\n    return self.foo()\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert!(hits.len() >= 2, "{:?}", hits);
    }
}
