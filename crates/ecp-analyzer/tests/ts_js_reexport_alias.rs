//! Re-export alias preservation for TS/JS (`export { X as Y } from 'lib'`).
//!
//! Verifies the parsers emit a RawImport with `imported_name=X`, `alias=Some(Y)`,
//! `source="lib"` so downstream alias-aware passes can follow the re-export chain.

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawImport;

fn find<'a>(imports: &'a [RawImport], imported_name: &str, source: &str) -> Vec<&'a RawImport> {
    imports
        .iter()
        .filter(|i| i.imported_name == imported_name && i.source == source)
        .collect()
}

#[test]
fn ts_reexport_aliased_named_specifier_preserves_alias() {
    let src = "export { foo as bar } from 'lib';";
    let provider = TypeScriptProvider::new().unwrap();
    let local = provider
        .parse_file("test.ts".as_ref(), src.as_bytes())
        .unwrap();

    let matches = find(&local.imports, "foo", "lib");
    assert_eq!(matches.len(), 1, "imports: {:?}", local.imports);
    assert_eq!(matches[0].alias.as_deref(), Some("bar"));
}

#[test]
fn ts_reexport_unaliased_named_specifier_alias_is_none() {
    let src = "export { foo } from 'lib';";
    let provider = TypeScriptProvider::new().unwrap();
    let local = provider
        .parse_file("test.ts".as_ref(), src.as_bytes())
        .unwrap();

    let matches = find(&local.imports, "foo", "lib");
    assert_eq!(matches.len(), 1, "imports: {:?}", local.imports);
    assert_eq!(matches[0].alias, None);
}

#[test]
fn ts_reexport_namespace_emits_star_with_alias() {
    let src = "export * as ns from 'lib';";
    let provider = TypeScriptProvider::new().unwrap();
    let local = provider
        .parse_file("test.ts".as_ref(), src.as_bytes())
        .unwrap();

    let matches = find(&local.imports, "*", "lib");
    assert_eq!(matches.len(), 1, "imports: {:?}", local.imports);
    assert_eq!(matches[0].alias.as_deref(), Some("ns"));
}

#[test]
fn ts_reexport_multiple_specifiers_each_emit_with_alias() {
    let src = "export { a as A, b as B } from 'lib';";
    let provider = TypeScriptProvider::new().unwrap();
    let local = provider
        .parse_file("test.ts".as_ref(), src.as_bytes())
        .unwrap();

    let a_matches = find(&local.imports, "a", "lib");
    let b_matches = find(&local.imports, "b", "lib");
    assert_eq!(a_matches.len(), 1, "imports: {:?}", local.imports);
    assert_eq!(b_matches.len(), 1, "imports: {:?}", local.imports);
    assert_eq!(a_matches[0].alias.as_deref(), Some("A"));
    assert_eq!(b_matches[0].alias.as_deref(), Some("B"));
}

#[test]
fn js_reexport_aliased_named_specifier_preserves_alias() {
    let src = "export { foo as bar } from 'lib';";
    let provider = JavaScriptProvider::new().unwrap();
    let local = provider
        .parse_file("test.js".as_ref(), src.as_bytes())
        .unwrap();

    let matches = find(&local.imports, "foo", "lib");
    assert_eq!(matches.len(), 1, "imports: {:?}", local.imports);
    assert_eq!(matches[0].alias.as_deref(), Some("bar"));
}

#[test]
fn js_reexport_unaliased_named_specifier_alias_is_none() {
    let src = "export { foo } from 'lib';";
    let provider = JavaScriptProvider::new().unwrap();
    let local = provider
        .parse_file("test.js".as_ref(), src.as_bytes())
        .unwrap();

    let matches = find(&local.imports, "foo", "lib");
    assert_eq!(matches.len(), 1, "imports: {:?}", local.imports);
    assert_eq!(matches[0].alias, None);
}
