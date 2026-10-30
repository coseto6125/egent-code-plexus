//! Rust parser fixes: nested-use import sources and turbofish calls.

use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{CallSite, LocalGraph};
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    RustProvider::new()
        .expect("RustProvider::new")
        .parse_file(Path::new("a.rs"), src.as_bytes())
        .expect("parse_file")
}

/// `(source, imported_name, alias)` of every import, in emitted order.
fn imports(g: &LocalGraph) -> Vec<(&str, &str, Option<&str>)> {
    g.imports
        .iter()
        .map(|i| {
            (
                i.source.as_str(),
                i.imported_name.as_str(),
                i.alias.as_deref(),
            )
        })
        .collect()
}

fn callee_names<'g>(g: &'g LocalGraph, caller: &str) -> Vec<&'g str> {
    g.nodes
        .iter()
        .find(|n| n.name == caller)
        .unwrap_or_else(|| panic!("no node {caller}"))
        .calls
        .iter()
        .map(|raw| CallSite::parse(raw).fallback().name())
        .collect()
}

// FU-2026-10-30-ca05646baa7b
#[test]
fn test_rust_nested_use_alias_in_nested_list_source_has_no_trailing_separator() {
    let g = parse("use a::{b::{c as d}};\n");
    let got = imports(&g);
    assert!(
        got.contains(&("a::b", "c", Some("d"))),
        "expected source a::b for `c as d`, got {got:?}"
    );
    assert!(
        got.iter().all(|(source, ..)| !source.ends_with("::")),
        "{got:?}"
    );
}

// FU-2026-10-30-ca05646baa7b
#[test]
fn test_rust_nested_use_alias_two_levels_deep_keeps_full_module_path() {
    let g = parse("use a::{b::{c::{e as f}}};\n");
    let got = imports(&g);
    assert!(got.contains(&("a::b::c", "e", Some("f"))), "{got:?}");
    assert!(
        got.iter().all(|(source, ..)| !source.ends_with("::")),
        "{got:?}"
    );
}

// FU-2026-10-30-ca05646baa7b
#[test]
fn test_rust_nested_use_alias_beside_plain_item_emits_both() {
    let g = parse("use a::{b::{c as d, e}};\n");
    let got = imports(&g);
    assert!(got.contains(&("a::b", "c", Some("d"))), "{got:?}");
    assert!(got.contains(&("a::b", "e", None)), "{got:?}");
    assert!(
        got.iter().all(|(source, ..)| !source.ends_with("::")),
        "{got:?}"
    );
}

// FU-2026-10-30-ca05646baa7b
#[test]
fn test_rust_use_alias_in_flat_list_stays_one_import() {
    let g = parse("use a::{b as d};\n");
    let got = imports(&g);
    assert_eq!(
        got.iter().filter(|(_, name, _)| *name == "b").count(),
        1,
        "{got:?}"
    );
    assert!(got.contains(&("a", "b", Some("d"))), "{got:?}");
}

// FU-2026-10-30-c017b835ee81
#[test]
fn test_rust_turbofish_call_followed_by_try_gets_calls_entry() {
    let g = parse("fn b<T>() -> Option<T> { None }\nfn a() -> Option<u8> { b::<u8>()?; None }\n");
    assert_eq!(callee_names(&g, "a"), ["b"]);
}

// FU-2026-10-30-c017b835ee81
#[test]
fn test_rust_scoped_turbofish_call_followed_by_try_keeps_path() {
    let g = parse("fn a() -> Option<u8> { m::b::<u8>(1)?; None }\n");
    let names = callee_names(&g, "a");
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].ends_with('b'), "{names:?}");
}

// FU-2026-10-30-c017b835ee81
#[test]
fn test_rust_turbofish_call_without_try_drops_type_arguments_from_callee() {
    let g = parse("fn b<T>() {}\nfn a() { b::<u8>(); }\n");
    assert_eq!(callee_names(&g, "a"), ["b"]);
}

// FU-2026-10-30-c017b835ee81
#[test]
fn test_rust_plain_call_followed_by_try_keeps_calls_entry() {
    let g = parse("fn b() -> Option<u8> { None }\nfn a() -> Option<u8> { b()?; None }\n");
    assert_eq!(callee_names(&g, "a"), ["b"]);
}

// FU-2026-10-30-c017b835ee81 sibling: method-call turbofish.
#[test]
fn test_rust_method_turbofish_call_drops_type_arguments_from_callee() {
    let g = parse("fn a(x: Foo) { x.m::<u8>(); }\n");
    let names = callee_names(&g, "a");
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(!names[0].contains('<'), "{names:?}");
}
