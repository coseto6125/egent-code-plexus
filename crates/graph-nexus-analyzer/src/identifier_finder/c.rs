//! C identifier finder. Covers:
//! - `identifier` (variables, function names, parameter names)
//! - `type_identifier` (typedef'd type names referenced at use sites)
//! - `field_identifier` (struct/union field access / declaration)

use super::generic::find_by_kinds;
use graph_nexus_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["identifier", "type_identifier", "field_identifier"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_c::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_function_and_calls() {
        let src = b"int foo() { return 0; }\nint main() { return foo() + foo(); }\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn finds_struct_field() {
        let src = b"struct S { int foo; };\nint read(struct S* s) { return s->foo; }\n";
        let hits = find_identifier_occurrences(src, "foo");
        // field decl + arrow-access = 2
        assert_eq!(hits.len(), 2, "{:?}", hits);
    }

    #[test]
    fn ignores_string_literals() {
        let src = b"const char* s = \"foo\";\nint foo = 1;\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 1, "{:?}", hits);
    }
}
