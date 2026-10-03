//! Rust `use` trees: every imported item becomes one `RawImport` with the
//! module path as `source`, whatever the nesting. The resolver binds a name
//! only through an import on record, so a dropped `use a::{b::C}` leaves `C`
//! to the global tier, which refuses common names or guesses a same-named
//! project item.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::RelType;
use std::path::Path;

/// `(source, imported_name, alias)` of every import in `src`, sorted.
fn imports(src: &str) -> Vec<(String, String, Option<String>)> {
    let graph = RustProvider::new()
        .expect("RustProvider::new")
        .parse_file(Path::new("src/app.rs"), src.as_bytes())
        .expect("parse_file");
    let mut out: Vec<_> = graph
        .imports
        .into_iter()
        .map(|i| (i.source, i.imported_name, i.alias))
        .collect();
    out.sort();
    out
}

fn plain(source: &str, name: &str) -> (String, String, Option<String>) {
    (source.to_string(), name.to_string(), None)
}

#[test]
fn test_use_nested_path_in_list_records_its_module() {
    assert_eq!(
        imports("use ext::{assert_ok, io::Builder};\n"),
        vec![plain("ext", "assert_ok"), plain("ext::io", "Builder")]
    );
}

#[test]
fn test_use_deeply_nested_lists_record_every_item() {
    assert_eq!(
        imports("use crate::{a::{b, c::D}, e};\n"),
        vec![
            plain("crate", "e"),
            plain("crate::a", "b"),
            plain("crate::a::c", "D"),
        ]
    );
}

#[test]
fn test_use_self_in_list_records_the_module_itself() {
    assert_eq!(
        imports("use crate::x::{self, y};\n"),
        vec![plain("crate", "x"), plain("crate::x", "y")]
    );
}

#[test]
fn test_use_glob_records_star_from_its_module() {
    assert_eq!(
        imports("use super::bignum::*;\n"),
        vec![plain("super::bignum", "*")]
    );
    assert_eq!(
        imports("use crate::{a::*, b};\n"),
        vec![plain("crate", "b"), plain("crate::a", "*")]
    );
}

#[test]
fn test_use_aliased_nested_path_keeps_alias() {
    assert_eq!(
        imports("use crate::{a::B as C};\n"),
        vec![(
            "crate::a".to_string(),
            "B".to_string(),
            Some("C".to_string())
        )]
    );
}

#[test]
fn test_use_top_level_list_records_each_item() {
    assert_eq!(
        imports("use {a::B, c};\n"),
        vec![plain("a", "B"), plain("c", "c")]
    );
}

/// The forms the import query already handles stay one record each.
#[test]
fn test_use_flat_forms_are_not_recorded_twice() {
    assert_eq!(
        imports("use std::collections::{HashMap, HashSet as Set};\n"),
        vec![
            plain("std::collections", "HashMap"),
            (
                "std::collections".to_string(),
                "HashSet".to_string(),
                Some("Set".to_string())
            ),
        ]
    );
    assert_eq!(imports("use a::b::C;\n"), vec![plain("a::b", "C")]);
    assert_eq!(imports("use a;\n"), vec![plain("a", "a")]);
}

#[test]
fn test_use_inside_inline_module_is_recorded() {
    assert_eq!(
        imports("mod m {\n    use crate::{x::Y};\n}\n"),
        vec![plain("crate::x", "Y")]
    );
}

/// End to end: `use crate::{model::Repo}` binds `Repo::new` to the imported
/// module, not to a same-named `Repo` elsewhere.
#[test]
fn test_nested_use_binds_qualified_call_to_imported_module() {
    let provider = RustProvider::new().expect("RustProvider::new");
    let repo =
        "pub struct Repo;\nimpl Repo {\n    pub fn new() -> Repo {\n        Repo\n    }\n}\n";
    let mut builder = GraphBuilder::new();
    for (path, src) in [
        ("src/lib.rs", "mod model;\nmod other;\nmod app;\n"),
        ("src/model.rs", repo),
        ("src/other.rs", repo),
        (
            "src/app.rs",
            "use crate::{model::Repo};\n\npub fn run() {\n    Repo::new();\n}\n",
        ),
    ] {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), src.as_bytes())
                .expect("parse_file"),
        );
    }
    let graph = builder.build();
    let pool = graph.string_pool.as_slice();
    let targets: Vec<String> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == "run")
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == "new")
        .map(|e| {
            let file = graph.nodes[e.target as usize].file_idx as usize;
            graph.files[file].path.resolve(pool).to_string()
        })
        .collect();
    assert_eq!(targets, vec!["src/model.rs".to_string()]);
}
