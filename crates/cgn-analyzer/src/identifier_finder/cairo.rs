//! Cairo identifier finder. tree-sitter-cairo uses `name` as the identifier
//! node kind (not `identifier`), as seen in its queries.scm.

use super::generic::find_by_kinds;
use cgn_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["name", "identifier"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_cairo::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_fn_def_and_call() {
        let src = b"fn foo() {}\nfn main() { foo(); }\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert!(hits.len() >= 2, "{:?}", hits);
    }
}
