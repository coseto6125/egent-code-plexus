//! PHP identifier finder. Tree-sitter-php uses:
//! - `name` for function names, class names, method names, namespaces
//! - `variable_name` for `$foo` variables (the whole `$foo` token, not
//!   just `foo`) — we DON'T include this here because renaming `foo`
//!   would have to deal with the leading `$` and the user almost
//!   certainly means the symbol, not all `$foo` usages.
//!
//! PHP rename limitation: variables (`$foo`) are not renamed by this
//! pass. Symbols at function / class / method / namespace level work.

use super::generic::find_by_kinds_skipping;
use graph_nexus_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["name"];
// `variable_name` wraps `$foo` and contains a `name` child whose text is
// `foo` without the `$`. Skipping the subtree prevents `gnx rename foo`
// from accidentally rewriting every `$foo` variable. Variable rename is
// a separate concern outside this finder's scope.
const SKIP: &[&str] = &["variable_name"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds_skipping(
        source,
        target_name,
        &tree_sitter_php::LANGUAGE_PHP.into(),
        KINDS,
        SKIP,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_function_and_calls() {
        let src = b"<?php\nfunction foo() {}\nfoo(); foo();\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn finds_class_decl_and_use() {
        let src = b"<?php\nclass Foo {}\nnew Foo();\n";
        let hits = find_identifier_occurrences(src, "Foo");
        assert_eq!(hits.len(), 2, "{:?}", hits);
    }

    #[test]
    fn variable_dollar_not_renamed() {
        // `$foo` variable usage is intentionally NOT in the kind set —
        // documented limitation.
        let src = b"<?php\n$foo = 1;\nfunction foo() {}\nfoo();\n";
        let hits = find_identifier_occurrences(src, "foo");
        // function decl + call site = 2 (the $foo variable should NOT match).
        assert_eq!(hits.len(), 2, "{:?}", hits);
    }
}
