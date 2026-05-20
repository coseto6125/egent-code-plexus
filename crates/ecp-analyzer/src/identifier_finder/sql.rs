//! SQL identifier finder. tree-sitter-sequel exposes table/column names
//! as `identifier` (sometimes wrapped in `object_reference` for qualified
//! names; the inner identifier still matches).

use super::generic::find_by_kinds;
use ecp_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["identifier"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_sequel::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_table_in_create_and_select() {
        let src = b"CREATE TABLE users (id INT);\nSELECT * FROM users;\n";
        let hits = find_identifier_occurrences(src, "users");
        assert!(hits.len() >= 2, "{:?}", hits);
    }
}
