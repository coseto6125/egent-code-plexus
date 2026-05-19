//! HCL identifier finder.

use super::generic::find_by_kinds;
use cgn_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["identifier"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_hcl::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_resource_label_reference() {
        let src = b"resource \"aws_instance\" \"foo\" {}\noutput \"x\" { value = foo.id }\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert!(!hits.is_empty(), "{:?}", hits);
    }
}
