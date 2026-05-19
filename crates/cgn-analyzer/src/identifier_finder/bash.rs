//! Bash identifier finder. tree-sitter-bash names function identifiers as
//! `word` and variable references as `variable_name`. Both are renameable
//! surfaces for `cgn rename`.

use super::generic::find_by_kinds;
use cgn_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["word", "variable_name"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_bash::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_function_def_and_call() {
        let src = b"foo() { echo hi; }\nfoo\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 2, "{:?}", hits);
    }

    #[test]
    fn finds_variable_reference() {
        let src = b"BAR=1\necho $BAR\n";
        let hits = find_identifier_occurrences(src, "BAR");
        assert!(!hits.is_empty(), "expected at least 1, got {:?}", hits);
    }
}
