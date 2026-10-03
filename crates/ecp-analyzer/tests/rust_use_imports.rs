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
fn test_use_aliased_self_in_list_records_the_module_with_alias() {
    assert_eq!(
        imports("use crate::m::sub::{self as s};\n"),
        vec![(
            "crate::m".to_string(),
            "sub".to_string(),
            Some("s".to_string())
        )]
    );
}

/// `use ::{a, b as c}` has no path, so the import query does not read it.
#[test]
fn test_use_pathless_scoped_list_records_each_item() {
    assert_eq!(
        imports("use ::{std, core as c};\n"),
        vec![
            (
                "core".to_string(),
                "core".to_string(),
                Some("c".to_string())
            ),
            plain("std", "std"),
        ]
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

/// Callee files of `go`'s calls to `run` in a crate where the parent module
/// `m` and its child `sub` both define `run`.
fn run_targets(app: &str, sub_path: &str) -> Vec<String> {
    let provider = RustProvider::new().expect("RustProvider::new");
    let mut builder = GraphBuilder::new();
    for (path, src) in [
        ("crates/x/src/lib.rs", "pub mod m;\npub mod app;\n"),
        ("crates/x/src/m/mod.rs", "pub mod sub;\npub fn run() {}\n"),
        (sub_path, "pub fn run() {}\n"),
        ("crates/x/src/app.rs", app),
    ] {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), src.as_bytes())
                .expect("parse_file"),
        );
    }
    let graph = builder.build();
    let pool = graph.string_pool.as_slice();
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == "go")
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == "run")
        .map(|e| {
            let file = graph.nodes[e.target as usize].file_idx as usize;
            graph.files[file].path.resolve(pool).to_string()
        })
        .collect()
}

/// `use crate::m::sub; sub::run()` calls the module `sub`'s own `run`, not
/// the parent file that declares `mod sub;` and happens to define `run`.
#[test]
fn test_module_import_qualifier_resolves_in_the_module_file() {
    for app in [
        "use crate::m::sub;\npub fn go() { sub::run(); }\n",
        "use crate::m::sub::{self};\npub fn go() { sub::run(); }\n",
    ] {
        assert_eq!(
            run_targets(app, "crates/x/src/m/sub.rs"),
            vec!["crates/x/src/m/sub.rs".to_string()],
            "{app}"
        );
    }
    assert_eq!(
        run_targets(
            "use crate::m::sub;\npub fn go() { sub::run(); }\n",
            "crates/x/src/m/sub/mod.rs"
        ),
        vec!["crates/x/src/m/sub/mod.rs".to_string()]
    );
}

/// `sub.rs` exists but has no `run`: the qualifier is still the module, so
/// the parent's own `run` is not the target.
#[test]
fn test_module_import_qualifier_missing_member_does_not_fall_back_to_parent() {
    let provider = RustProvider::new().expect("RustProvider::new");
    let mut builder = GraphBuilder::new();
    for (path, src) in [
        ("crates/x/src/lib.rs", "pub mod m;\npub mod app;\n"),
        ("crates/x/src/m/mod.rs", "pub mod sub;\npub fn run() {}\n"),
        ("crates/x/src/m/sub.rs", "pub fn other() {}\n"),
        (
            "crates/x/src/app.rs",
            "use crate::m::sub;\npub fn go() { sub::run(); }\n",
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
    let parent_hits = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == "go")
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == "run")
        .count();
    assert_eq!(parent_hits, 0);
}

/// Calls edges from `go` in a workspace written to disk, so the module tree
/// (built from Cargo.toml) takes part. Every target file is returned.
fn workspace_go_targets(files: &[(&str, &str)], callee: &str) -> Vec<String> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let provider = RustProvider::new().expect("RustProvider::new");
    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    for (rel, src) in files {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, src).unwrap();
        if rel.ends_with(".rs") {
            builder.add_graph(
                provider
                    .parse_file(Path::new(rel), src.as_bytes())
                    .expect("parse_file"),
            );
        }
    }
    let graph = builder.build();
    let pool = graph.string_pool.as_slice();
    let mut out: Vec<String> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == "go")
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == callee)
        .map(|e| {
            let file = graph.nodes[e.target as usize].file_idx as usize;
            graph.files[file].path.resolve(pool).to_string()
        })
        .collect();
    out.sort();
    out
}

/// A bin imports its own lib by the `[lib] name` (`egent-code-plexus` is
/// written `ecp_cli`). The test file sharing the module's stem is a decoy a
/// stem-based guess would bind to.
#[test]
fn test_lib_name_import_qualifier_resolves_through_the_module_tree() {
    let manifest = (
        "crates/app/Cargo.toml",
        "[package]\nname = \"my-app\"\n\n[lib]\nname = \"app_core\"\npath = \"src/lib.rs\"\n",
    );
    let common = [
        ("Cargo.toml", "[workspace]\nmembers = [\"crates/app\"]\n"),
        manifest,
        ("crates/app/src/lib.rs", "pub mod auto;\n"),
        ("crates/app/src/auto.rs", "pub fn ensure() {}\n"),
        ("crates/app/tests/auto.rs", "pub fn ensure() {}\n"),
    ];
    for main in [
        "use app_core::{auto};\nfn go() { auto::ensure(); }\n",
        "use app_core::auto;\nfn go() { auto::ensure(); }\n",
        "fn go() { app_core::auto::ensure(); }\n",
    ] {
        let mut files = common.to_vec();
        files.push(("crates/app/src/main.rs", main));
        assert_eq!(
            workspace_go_targets(&files, "ensure"),
            vec!["crates/app/src/auto.rs".to_string()],
            "{main}"
        );
    }
}

/// The same expansion for a dependency crate named by its package name.
#[test]
fn test_cross_crate_module_import_qualifier_resolves() {
    let files = [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/app\", \"crates/other\"]\n",
        ),
        ("crates/app/Cargo.toml", "[package]\nname = \"app\"\n"),
        (
            "crates/app/src/lib.rs",
            "use other::registry;\npub fn go() { registry::lookup(); }\n",
        ),
        ("crates/other/Cargo.toml", "[package]\nname = \"other\"\n"),
        ("crates/other/src/lib.rs", "pub mod registry;\n"),
        ("crates/other/src/registry.rs", "pub fn lookup() {}\n"),
        ("crates/app/src/registry.rs", "pub fn lookup() {}\n"),
    ];
    assert_eq!(
        workspace_go_targets(&files, "lookup"),
        vec!["crates/other/src/registry.rs".to_string()]
    );
}
