//! Zig identifier finder.

use super::generic::find_by_kinds;
use graph_nexus_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["identifier"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_zig::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_fn_def_and_call() {
        let src = b"fn foo() void {}\nfn main() void { foo(); }\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert!(hits.len() >= 2, "{:?}", hits);
    }
}
