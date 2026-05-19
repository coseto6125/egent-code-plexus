//! Go identifier finder. Covers:
//! - `identifier` (variables, functions, parameters)
//! - `type_identifier` (struct/interface/alias names, type references)
//! - `field_identifier` (struct field access / declaration)
//! - `package_identifier` (qualified package access like `pkg.Foo`)

use super::generic::find_by_kinds;
use cgn_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &[
    "identifier",
    "type_identifier",
    "field_identifier",
    "package_identifier",
];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(source, target_name, &tree_sitter_go::LANGUAGE.into(), KINDS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_func_and_calls() {
        let src = b"package p\nfunc Foo() {}\nfunc main() { Foo(); Foo() }\n";
        let hits = find_identifier_occurrences(src, "Foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn finds_struct_decl_and_field_access() {
        let src = b"package p\ntype Foo struct { foo int }\nfunc m(f Foo) int { return f.foo }\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 2, "{:?}", hits);
    }

    #[test]
    fn ignores_string_literals() {
        let src = b"package p\nfunc m() { s := \"foo\"; foo := 1; _ = s; _ = foo }\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 2, "{:?}", hits);
    }
}
