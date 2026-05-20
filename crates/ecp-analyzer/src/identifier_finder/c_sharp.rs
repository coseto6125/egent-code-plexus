//! C# identifier finder. Tree-sitter-c-sharp uses `identifier` for almost
//! every identifier-position node (classes, methods, fields, variables,
//! type references). No separate `type_identifier` kind.

use super::generic::find_by_kinds;
use ecp_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &["identifier"];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_c_sharp::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_class_decl_and_use() {
        let src = b"class Foo {}\nclass Bar { Foo Make() => new Foo(); }\n";
        let hits = find_identifier_occurrences(src, "Foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn finds_method_call() {
        let src = b"class C { void Foo() {} void Run() { Foo(); Foo(); } }\n";
        let hits = find_identifier_occurrences(src, "Foo");
        assert_eq!(hits.len(), 3, "{:?}", hits);
    }

    #[test]
    fn ignores_string_literals() {
        let src = b"class C { void M() { string s = \"foo\"; int foo = 1; } }\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert_eq!(hits.len(), 1, "{:?}", hits);
    }
}
