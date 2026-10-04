//! Rust `use` trees: every imported item becomes one `RawImport` with the
//! module path as `source`, whatever the nesting. The resolver binds a name
//! only through an import on record, so a dropped `use a::{b::C}` leaves `C`
//! to the global tier, which refuses common names or guesses a same-named
//! project item.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType};
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
    workspace_go_target_kinds(files, callee)
        .into_iter()
        .map(|(file, _)| file)
        .collect()
}

/// [`workspace_go_targets`] with each target's kind, for a callee name that
/// a free `fn` and a method share in one file.
fn workspace_go_target_kinds(files: &[(&str, &str)], callee: &str) -> Vec<(String, NodeKind)> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let rust = RustProvider::new().expect("RustProvider::new");
    let python = PythonProvider::new().expect("PythonProvider::new");
    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    for (rel, src) in files {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, src).unwrap();
        let provider: &dyn LanguageProvider = match Path::new(rel).extension() {
            Some(ext) if ext == "rs" => &rust,
            Some(ext) if ext == "py" => &python,
            _ => continue,
        };
        builder.add_graph(
            provider
                .parse_file(Path::new(rel), src.as_bytes())
                .expect("parse_file"),
        );
    }
    let graph = builder.build();
    let pool = graph.string_pool.as_slice();
    let mut out: Vec<(String, NodeKind)> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == "go")
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == callee)
        .map(|e| {
            let target = &graph.nodes[e.target as usize];
            let file = target.file_idx as usize;
            (
                graph.files[file].path.resolve(pool).to_string(),
                target.kind,
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
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

fn two_crates(
    app_lib: &str,
    other_files: &[(&'static str, &'static str)],
) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = vec![
        (
            "Cargo.toml".into(),
            "[workspace]\nmembers = [\"crates/app\", \"crates/other\"]\n".into(),
        ),
        (
            "crates/app/Cargo.toml".into(),
            "[package]\nname = \"app\"\n".into(),
        ),
        ("crates/app/src/lib.rs".into(), app_lib.into()),
        (
            "crates/other/Cargo.toml".into(),
            "[package]\nname = \"other\"\n".into(),
        ),
    ];
    files.extend(
        other_files
            .iter()
            .map(|(p, s)| (p.to_string(), s.to_string())),
    );
    files
}

fn targets_of(files: &[(String, String)], callee: &str) -> Vec<String> {
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    workspace_go_targets(&borrowed, callee)
}

/// `use other::Widget; Widget::new()` imports a type, not a module: the
/// crate-name module branch must leave it to the tiers that resolve it.
#[test]
fn test_cross_crate_type_import_qualifier_still_resolves() {
    let files = two_crates(
        "use other::Widget;\npub fn go() { Widget::new(); }\n",
        &[(
            "crates/other/src/lib.rs",
            "pub struct Widget;\nimpl Widget { pub fn new() -> Self { Widget } }\n",
        )],
    );
    assert_eq!(
        targets_of(&files, "new"),
        vec!["crates/other/src/lib.rs".to_string()]
    );
}

/// A renamed re-export inside the imported module still names that module, so
/// the caller crate's own `registry.rs` is a decoy that must not bind. The
/// qualifier tier hands back a file and the caller looks `lookup` up in it, so
/// the edge to `lookup_impl` is not made either: a missed edge, not a wrong one.
#[test]
fn test_module_import_through_renamed_reexport_never_binds_the_decoy() {
    let mut files = two_crates(
        "pub mod registry;\nuse other::registry;\npub fn go() { registry::lookup(); }\n",
        &[
            ("crates/other/src/lib.rs", "pub mod registry;\n"),
            (
                "crates/other/src/registry.rs",
                "mod imp;\npub use imp::lookup_impl as lookup;\n",
            ),
            (
                "crates/other/src/registry/imp.rs",
                "pub fn lookup_impl() {}\n",
            ),
        ],
    );
    files.push((
        "crates/app/src/registry.rs".into(),
        "pub fn lookup() {}\n".into(),
    ));
    assert!(
        targets_of(&files, "lookup").is_empty(),
        "the decoy must not bind"
    );
}

/// `mod utils; use utils::fs;` names the caller's own module even when a
/// workspace crate is also called `utils`.
#[test]
fn test_local_module_shadows_a_same_named_workspace_crate() {
    let files: Vec<(String, String)> = [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/app\", \"crates/utils\"]\n",
        ),
        ("crates/app/Cargo.toml", "[package]\nname = \"app\"\n"),
        (
            "crates/app/src/lib.rs",
            "mod utils;\nuse utils::fs;\npub fn go() { fs::read_all(); }\n",
        ),
        ("crates/app/src/utils.rs", "pub mod fs;\n"),
        ("crates/app/src/utils/fs.rs", "pub fn read_all() {}\n"),
        ("crates/utils/Cargo.toml", "[package]\nname = \"utils\"\n"),
        ("crates/utils/src/lib.rs", "pub mod fs;\n"),
        ("crates/utils/src/fs.rs", "pub fn read_all() {}\n"),
    ]
    .iter()
    .map(|(p, s)| (p.to_string(), s.to_string()))
    .collect();
    assert!(
        !targets_of(&files, "read_all").contains(&"crates/utils/src/fs.rs".to_string()),
        "the workspace crate `utils` is not what `mod utils;` names"
    );
}

/// A PyO3-style repo: the Rust lib is named like the Python package. A Python
/// call through `from mylib import utils` must not bind into the Rust crate.
#[test]
fn test_python_import_does_not_bind_into_a_same_named_rust_crate() {
    let files: Vec<(String, String)> = [
        (
            "Cargo.toml",
            "[package]\nname = \"mylib-rs\"\n\n[lib]\nname = \"mylib\"\n",
        ),
        ("src/lib.rs", "mod utils;\n"),
        ("src/utils.rs", "pub fn helper() {}\n"),
        ("python/mylib/__init__.py", ""),
        ("python/mylib/utils.py", "def helper():\n    pass\n"),
        (
            "tests/test_x.py",
            "from mylib import utils\n\ndef go():\n    utils.helper()\n",
        ),
    ]
    .iter()
    .map(|(p, s)| (p.to_string(), s.to_string()))
    .collect();
    assert!(
        !targets_of(&files, "helper").contains(&"src/utils.rs".to_string()),
        "a Python call must not reach Rust: {:?}",
        targets_of(&files, "helper")
    );
}

/// Method syntax never calls a free function in Rust, so `n.name.resolve(1)`
/// on a value of an unindexed type must not bind to a same-file `fn resolve`
/// (FU-2026-10-03-65d39a6471ea).
#[test]
fn test_method_call_never_binds_a_same_file_free_function() {
    let files = [(
        "src/lib.rs",
        "pub fn resolve(p: u32) -> u32 { p }\n\
         pub fn go(n: &ext::Node) { n.name.resolve(1); }\n",
    )];
    assert_eq!(
        workspace_go_targets(&files, "resolve"),
        Vec::<String>::new()
    );
}

/// `de::Error::custom` names `Error` inside module `de` (here serde's). A
/// project `Error` declared elsewhere is another item, whether it sits in the
/// caller's file (Tier 1) or is the only `Error` in the repo (Tier 3)
/// (FU-2026-10-03-96ccd9c59f1e).
#[test]
fn test_module_qualified_type_call_never_binds_an_unrelated_type() {
    let same_file = [(
        "src/lib.rs",
        "use serde::de;\n\
         pub struct Error;\n\
         impl Error { pub fn custom() {} }\n\
         pub fn go() { de::Error::custom(); }\n",
    )];
    assert_eq!(
        workspace_go_targets(&same_file, "custom"),
        Vec::<String>::new(),
        "same file"
    );
    let unique_global = [
        (
            "src/lib.rs",
            "pub mod error;\nuse serde::de;\npub fn go() { de::Error::custom(); }\n",
        ),
        (
            "src/error.rs",
            "pub struct Error;\nimpl Error { pub fn custom() {} }\n",
        ),
    ];
    assert_eq!(
        workspace_go_targets(&unique_global, "custom"),
        Vec::<String>::new(),
        "unique global"
    );
}

/// A crate with both `src/lib.rs` and `src/main.rs`: `crate::` in a module
/// that `main.rs` declares names the bin crate's root, not the lib's
/// (FU-2026-10-03-95b377b1fafe).
#[test]
fn test_crate_path_in_a_bin_module_resolves_against_main_rs() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"tool\"\n"),
        ("src/lib.rs", "pub fn helper() {}\n"),
        (
            "src/main.rs",
            "mod cli;\npub fn helper() {}\nfn main() {}\n",
        ),
        (
            "src/cli.rs",
            "use crate::helper;\npub fn go() { helper(); }\n",
        ),
    ];
    assert_eq!(
        workspace_go_targets(&files, "helper"),
        vec!["src/main.rs".to_string()]
    );
}

/// `super` inside an inline `mod tests { }` names the file's own module, not
/// the file's parent (FU-2026-10-03-0b829952a712).
#[test]
fn test_super_in_an_inline_module_names_the_enclosing_file_module() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"tool\"\n"),
        ("src/lib.rs", "pub mod inner;\npub fn helper() {}\n"),
        (
            "src/inner.rs",
            "pub fn helper() {}\n\
             mod tests {\n    use super::helper;\n    pub fn go() { helper(); }\n}\n",
        ),
    ];
    assert_eq!(
        workspace_go_targets(&files, "helper"),
        vec!["src/inner.rs".to_string()]
    );
}

/// A `#[path]` module's `super` is the module that declares it, wherever the
/// file sits. The undeclared `src/imp.rs` is a decoy a layout-based guess
/// reaches (FU-2026-10-03-0b829952a712).
#[test]
fn test_super_in_a_path_attribute_module_names_the_declaring_module() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"tool\"\n"),
        (
            "src/lib.rs",
            "#[path = \"imp/real.rs\"]\nmod imp;\npub fn helper() {}\n",
        ),
        (
            "src/imp/real.rs",
            "use super::helper;\npub fn go() { helper(); }\n",
        ),
        ("src/imp.rs", "pub fn helper() {}\n"),
    ];
    assert_eq!(
        workspace_go_targets(&files, "helper"),
        vec!["src/lib.rs".to_string()]
    );
}

/// `self.resolve()` inside `impl S` is method syntax with a known receiver:
/// it binds the method, not the same-file free `fn resolve`, and keeps its
/// edge.
#[test]
fn test_method_call_on_self_binds_the_method_not_the_same_file_free_function() {
    let files = [(
        "src/lib.rs",
        "pub fn resolve(p: u32) -> u32 { p }\n\
         pub struct S;\n\
         impl S {\n    pub fn resolve(&self) -> u32 { 0 }\n    pub fn go(&self) { self.resolve(); }\n}\n",
    )];
    assert_eq!(
        workspace_go_target_kinds(&files, "resolve"),
        vec![("src/lib.rs".to_string(), NodeKind::Method)]
    );
}

/// Path syntax still reaches a free `fn`: the method-syntax rule touches only
/// `.` calls.
#[test]
fn test_path_call_to_a_free_function_still_binds_it() {
    let bare = [(
        "src/lib.rs",
        "pub fn resolve() {}\npub fn go() { resolve(); }\n",
    )];
    assert_eq!(
        workspace_go_target_kinds(&bare, "resolve"),
        vec![("src/lib.rs".to_string(), NodeKind::Function)],
        "bare call"
    );
    let crate_path = [
        ("Cargo.toml", "[package]\nname = \"tool\"\n"),
        (
            "src/lib.rs",
            "pub mod util;\npub fn go() { crate::util::resolve(); }\n",
        ),
        ("src/util.rs", "pub fn resolve() {}\n"),
    ];
    assert_eq!(
        workspace_go_targets(&crate_path, "resolve"),
        vec!["src/util.rs".to_string()],
        "crate path"
    );
}

/// `de::Error::custom` where `de` is an inline module of the caller's file
/// holding `Error`: the module path agrees, so the call binds.
#[test]
fn test_module_qualified_type_call_binds_an_inline_module_type() {
    let files = [(
        "src/lib.rs",
        "mod de {\n    pub struct Error;\n    impl Error {\n        pub fn custom() {}\n    }\n}\n\
         pub fn go() { de::Error::custom(); }\n",
    )];
    assert_eq!(
        workspace_go_targets(&files, "custom"),
        vec!["src/lib.rs".to_string()]
    );
}

/// `error::Error::custom` where `mod error;` is a project module holding
/// `Error`: the module path names `src/error.rs`, so the call binds there.
#[test]
fn test_module_qualified_type_call_binds_the_project_module_type() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"tool\"\n"),
        (
            "src/lib.rs",
            "pub mod error;\npub fn go() { error::Error::custom(); }\n",
        ),
        (
            "src/error.rs",
            "pub struct Error;\nimpl Error {\n    pub fn custom() {}\n}\n",
        ),
    ];
    assert_eq!(
        workspace_go_targets(&files, "custom"),
        vec!["src/error.rs".to_string()]
    );
}

/// A crate with only `src/main.rs`: `crate::` names `main.rs`.
#[test]
fn test_crate_path_in_a_main_only_crate_resolves_against_main_rs() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"tool\"\n"),
        (
            "src/main.rs",
            "mod cli;\npub fn helper() {}\nfn main() {}\n",
        ),
        (
            "src/cli.rs",
            "use crate::helper;\npub fn go() { helper(); }\n",
        ),
    ];
    assert_eq!(
        workspace_go_targets(&files, "helper"),
        vec!["src/main.rs".to_string()]
    );
}

/// A `crate::helper()` call, not only a `use`, in a module only `main.rs`
/// declares names the bin crate's root.
#[test]
fn test_crate_path_call_in_a_bin_module_resolves_against_main_rs() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"tool\"\n"),
        ("src/lib.rs", "pub fn helper() {}\n"),
        (
            "src/main.rs",
            "mod cli;\npub fn helper() {}\nfn main() {}\n",
        ),
        ("src/cli.rs", "pub fn go() { crate::helper(); }\n"),
    ];
    assert_eq!(
        workspace_go_targets(&files, "helper"),
        vec!["src/main.rs".to_string()]
    );
}

/// A module both `lib.rs` and `main.rs` declare has two crate roots, and
/// one with no Cargo.toml has no module tree: both keep the file-layout
/// guess, which probes `lib.rs` first.
#[test]
fn test_crate_path_without_one_declaring_target_keeps_the_layout_guess() {
    let both_roots = [
        ("Cargo.toml", "[package]\nname = \"tool\"\n"),
        ("src/lib.rs", "pub mod cli;\npub fn helper() {}\n"),
        (
            "src/main.rs",
            "mod cli;\npub fn helper() {}\nfn main() {}\n",
        ),
        (
            "src/cli.rs",
            "use crate::helper;\npub fn go() { helper(); }\n",
        ),
    ];
    assert_eq!(
        workspace_go_targets(&both_roots, "helper"),
        vec!["src/lib.rs".to_string()],
        "both roots"
    );
    let no_manifest = &both_roots[1..];
    assert_eq!(
        workspace_go_targets(no_manifest, "helper"),
        vec!["src/lib.rs".to_string()],
        "no Cargo.toml"
    );
}

/// `super::super` from a `#[path]` module climbs the logical modules
/// (`a::b` → crate root), not the file's directories. The layout guess
/// climbs above `src/` and finds nothing; `src/a.rs` keeps the global tier
/// ambiguous.
#[test]
fn test_super_super_in_a_path_attribute_module_climbs_logical_modules() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"tool\"\n"),
        ("src/lib.rs", "mod a;\npub fn helper() {}\n"),
        (
            "src/a.rs",
            "#[path = \"b_impl.rs\"]\nmod b;\npub fn helper() {}\n",
        ),
        (
            "src/b_impl.rs",
            "use super::super::helper;\npub fn go() { helper(); }\n",
        ),
    ];
    assert_eq!(
        workspace_go_targets(&files, "helper"),
        vec!["src/lib.rs".to_string()]
    );
}

/// Method syntax narrows the result, not the ambiguity check: with a free fn
/// `join` and one project method `join`, `p.join(..)` on an unindexed type
/// stays ambiguous instead of binding the method. With only the method, the
/// unique match still binds as before.
#[test]
fn test_method_call_with_a_same_named_free_fn_stays_ambiguous() {
    let ambiguous = [
        (
            "src/lib.rs",
            "pub mod a;\npub mod b;\npub fn go(p: &std::path::Path) { p.join(\"x\"); }\n",
        ),
        ("src/a.rs", "pub fn join(x: &str) {}\n"),
        (
            "src/b.rs",
            "pub struct Set;\nimpl Set { pub fn join(&self, x: &str) {} }\n",
        ),
    ];
    assert_eq!(
        workspace_go_targets(&ambiguous, "join"),
        Vec::<String>::new()
    );
    let unique = [
        (
            "src/lib.rs",
            "pub mod b;\npub fn go(s: &b::Set) { s.join(\"x\"); }\n",
        ),
        (
            "src/b.rs",
            "pub struct Set;\nimpl Set { pub fn join(&self, x: &str) {} }\n",
        ),
    ];
    assert_eq!(
        workspace_go_targets(&unique, "join"),
        vec!["src/b.rs".to_string()]
    );
}

/// A type re-exported through a grouped `pub use lock::{a, FileLock};` sits
/// in the module the path names, through the re-export. A decoy `FileLock`
/// elsewhere keeps the bare name ambiguous, so only the path can pick it.
#[test]
fn test_module_qualified_type_through_grouped_pub_use_binds_its_definition() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"tool\"\n"),
        ("src/lib.rs", "pub mod registry;\npub mod other;\npub fn go() { crate::registry::FileLock::acquire(); }\n"),
        ("src/registry/mod.rs", "mod lock;\npub use lock::{\n    lock_within,\n    FileLock,\n};\n"),
        ("src/registry/lock.rs", "pub fn lock_within() {}\npub struct FileLock;\nimpl FileLock { pub fn acquire() {} }\n"),
        ("src/other.rs", "pub struct FileLock;\nimpl FileLock { pub fn acquire() {} }\n"),
    ];
    assert_eq!(
        workspace_go_targets(&files, "acquire"),
        vec!["src/registry/lock.rs".to_string()]
    );
}

/// The same re-export reached through another workspace crate's name.
#[test]
fn test_cross_crate_type_through_grouped_pub_use_binds_its_definition() {
    let files = two_crates(
        "pub fn go() { other::registry::FileLock::acquire(); }\n",
        &[
            ("crates/other/src/lib.rs", "pub mod registry;\n"),
            ("crates/other/src/registry/mod.rs", "mod lock;\npub use lock::{lock_within, FileLock as FileLock};\n"),
            (
                "crates/other/src/registry/lock.rs",
                "pub fn lock_within() {}\npub struct FileLock;\nimpl FileLock { pub fn acquire() {} }\n",
            ),
            ("crates/app/src/decoy.rs", "pub struct FileLock;\nimpl FileLock { pub fn acquire() {} }\n"),
        ],
    );
    assert_eq!(
        targets_of(&files, "acquire"),
        vec!["crates/other/src/registry/lock.rs".to_string()]
    );
}

/// `registry::FileLock` where `registry/mod.rs` only re-exports `FileLock`
/// with a plain `pub use lock::FileLock;`: the module path names the type
/// through the re-export, so the call binds its definition in `lock.rs`,
/// whether `registry` is declared in the caller's file or brought in by
/// `use crate::registry;`.
#[test]
fn test_module_qualified_type_through_plain_pub_use_binds_its_definition() {
    let common = [
        ("Cargo.toml", "[package]\nname = \"tool\"\n"),
        (
            "src/registry/mod.rs",
            "mod lock;\npub use lock::FileLock;\n",
        ),
        (
            "src/registry/lock.rs",
            "pub struct FileLock;\nimpl FileLock { pub fn acquire() {} }\n",
        ),
    ];
    let mut declared_here = common.to_vec();
    declared_here.push((
        "src/lib.rs",
        "pub mod registry;\npub fn go() { registry::FileLock::acquire(); }\n",
    ));
    assert_eq!(
        workspace_go_targets(&declared_here, "acquire"),
        vec!["src/registry/lock.rs".to_string()],
        "declared in the caller's file"
    );
    let mut imported = common.to_vec();
    imported.push(("src/lib.rs", "pub mod registry;\npub mod foo;\n"));
    imported.push((
        "src/foo.rs",
        "use crate::registry;\npub fn go() { registry::FileLock::acquire(); }\n",
    ));
    assert_eq!(
        workspace_go_targets(&imported, "acquire"),
        vec!["src/registry/lock.rs".to_string()],
        "brought in by use crate::registry"
    );
}

/// A same-file free `fn join` keeps method syntax on an unindexed type
/// ambiguous in the same-file tier too: `Path::new(".").join("x")` binds
/// neither `join`. With only the method in the file, an untyped `x.join()`
/// still binds it.
#[test]
fn test_method_call_with_a_same_file_free_fn_binds_neither() {
    let ambiguous = [(
        "src/lib.rs",
        "pub fn join(x: &str) {}\n\
         pub struct Local;\n\
         impl Local { pub fn join(&self) {} }\n\
         pub fn go() { std::path::Path::new(\".\").join(\"x\"); }\n",
    )];
    assert_eq!(
        workspace_go_target_kinds(&ambiguous, "join"),
        Vec::<(String, NodeKind)>::new()
    );
    let method_only = [(
        "src/lib.rs",
        "pub struct Local;\n\
         impl Local { pub fn join(&self) {} }\n\
         pub fn go() { let x = ext::make(); x.join(); }\n",
    )];
    assert_eq!(
        workspace_go_target_kinds(&method_only, "join"),
        vec![("src/lib.rs".to_string(), NodeKind::Method)]
    );
}

/// A bin root re-exports `f` from its own module (`pub use inner::f;`):
/// `crate::f()` in a module only `main.rs` declares follows that re-export
/// to `inner.rs`. The lib's `f` is a decoy.
#[test]
fn test_crate_path_in_a_bin_module_follows_the_bin_root_reexport() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"tool\"\n"),
        ("src/lib.rs", "pub fn f() {}\n"),
        (
            "src/main.rs",
            "mod cli;\nmod inner;\npub use inner::f;\nfn main() {}\n",
        ),
        ("src/inner.rs", "pub fn f() {}\n"),
        ("src/cli.rs", "pub fn go() { crate::f(); }\n"),
    ];
    assert_eq!(
        workspace_go_targets(&files, "f"),
        vec!["src/inner.rs".to_string()]
    );
}

/// A renamed re-export (`pub use imp::lookup_impl as lookup;`) and a private
/// `fn lookup` beside `lookup_impl`: the module tree places `lookup` at
/// `lookup_impl`, so the private `lookup` must not bind.
#[test]
fn test_module_import_through_renamed_reexport_never_binds_a_private_namesake() {
    let files = two_crates(
        "use other::registry;\npub fn go() { registry::lookup(); }\n",
        &[
            ("crates/other/src/lib.rs", "pub mod registry;\n"),
            (
                "crates/other/src/registry.rs",
                "mod imp;\npub use imp::lookup_impl as lookup;\n",
            ),
            (
                "crates/other/src/registry/imp.rs",
                "pub fn lookup_impl() {}\nfn lookup() {}\n",
            ),
        ],
    );
    assert!(
        targets_of(&files, "lookup").is_empty(),
        "the private namesake must not bind: {:?}",
        targets_of(&files, "lookup")
    );
}
