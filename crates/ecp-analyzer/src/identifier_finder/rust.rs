//! Rust identifier finder. Covers:
//! - `identifier` (functions, variables, modules, macros without `!`)
//! - `type_identifier` (structs, enums, traits, type aliases, generic params)
//! - `field_identifier` (struct field access / declaration)
//! - `shorthand_field_identifier` (`Foo { name }` struct init shorthand)

use super::generic::find_by_kinds;
use ecp_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &[
    "identifier",
    "type_identifier",
    "field_identifier",
    "shorthand_field_identifier",
];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_rust::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_fn_and_calls() {
        let src = b"fn foo() {}\nfn main() { foo(); foo(); }\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn finds_struct_and_use() {
        let src = b"struct Foo;\nfn make() -> Foo { Foo }\n";
        let hits = find_identifier_occurrences(src, "Foo");
        // decl + return type + ctor expression = 3 hits
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn finds_field_access() {
        let src = b"struct S { foo: u32 }\nfn read(s: &S) -> u32 { s.foo }\n";
        let hits = find_identifier_occurrences(src, "foo");
        // field decl + field access = 2 hits
        assert_eq!(hits.len(), 2, "{:?}", hits);
    }

    #[test]
    fn ignores_string_literals_and_comments() {
        let src = b"// foo in comment\nlet foo = \"foo\";\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 1, "{:?}", hits);
    }
}
