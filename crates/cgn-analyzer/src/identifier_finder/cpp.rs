//! C++ identifier finder. Covers:
//! - `identifier` (functions, variables, parameters, namespaces)
//! - `type_identifier` (classes, structs, typedefs)
//! - `field_identifier` (member access / declaration)
//! - `namespace_identifier` (`std::cout` — `std` is a namespace_identifier)

use super::generic::find_by_kinds;
use cgn_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &[
    "identifier",
    "type_identifier",
    "field_identifier",
    "namespace_identifier",
];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_cpp::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_class_decl_and_use() {
        let src = b"class Foo {};\nFoo make() { return Foo(); }\n";
        let hits = find_identifier_occurrences(src, "Foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn finds_method_and_calls() {
        let src = b"int foo() { return 0; }\nint main() { return foo() + foo(); }\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn ignores_string_literals() {
        let src = b"const char* s = \"foo\";\nint foo = 1;\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 1, "{:?}", hits);
    }
}
