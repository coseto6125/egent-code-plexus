//! Nim identifier finder.

use super::generic::find_by_kinds;
use ecp_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["identifier"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    let lang: tree_sitter::Language = tree_sitter_nim::language();
    find_by_kinds(source, target_name, &lang, KINDS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_proc_def_and_call() {
        let src = b"proc foo() = discard\nfoo()\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert!(hits.len() >= 2, "{:?}", hits);
    }
}
