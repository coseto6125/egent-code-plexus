use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use std::path::Path;

const IMPACT_MOD: &str = r#"
pub struct ImpactArgs { pub flag: bool }
impl ImpactArgs {
    pub fn walks_tests(&self) -> bool { self.flag }
}
"#;

const WALK_MOD: &str = r#"
pub struct ImpactArgs { pub depth: u32 }
impl ImpactArgs {
    pub fn walks_tests(&self) -> bool { self.depth > 0 }
}
"#;

fn build(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let provider = RustProvider::new().expect("RustProvider::new");
    let mut builder = GraphBuilder::new();
    for (path, src) in files {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), src.as_bytes())
                .expect("parse_file"),
        );
    }
    builder.build()
}

/// `build` plus one TypeScript file.
fn build_with_ts(rust: &[(String, &str)], ts_path: &str, ts_src: &str) -> ZeroCopyGraph {
    let provider = RustProvider::new().expect("RustProvider::new");
    let mut builder = GraphBuilder::new();
    for (path, src) in rust {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), src.as_bytes())
                .expect("parse_file"),
        );
    }
    let ts = TypeScriptProvider::new().expect("TypeScriptProvider::new");
    builder.add_graph(
        ts.parse_file(Path::new(ts_path), ts_src.as_bytes())
            .expect("parse_file"),
    );
    builder.build()
}

/// File paths of every `walks_tests` node that `run` calls.
fn callee_files_of_run(graph: &ZeroCopyGraph) -> Vec<String> {
    let pool = graph.string_pool.as_slice();
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == "run")
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == "walks_tests")
        .map(|e| {
            let file_idx = graph.nodes[e.target as usize].file_idx as usize;
            graph.files[file_idx].path.resolve(pool).to_string()
        })
        .collect()
}

#[test]
fn test_super_import_from_non_mod_file_resolves_to_parent_module_item() {
    let symbol_rs = r#"
use super::ImpactArgs;

fn run(args: &ImpactArgs) -> bool {
    args.walks_tests()
}
"#;
    let g = build(&[
        ("src/commands/impact/mod.rs", IMPACT_MOD),
        ("src/commands/walk/mod.rs", WALK_MOD),
        ("src/commands/impact/symbol.rs", symbol_rs),
    ]);
    assert_eq!(
        callee_files_of_run(&g),
        vec!["src/commands/impact/mod.rs".to_string()],
        "`super` from impact/symbol.rs is the impact module: exactly one edge, into impact/mod.rs"
    );
}

#[test]
fn test_super_brace_import_from_non_mod_file_resolves_to_parent_module_item() {
    let symbol_rs = r#"
use super::{ImpactArgs};

fn run(args: &ImpactArgs) -> bool {
    args.walks_tests()
}
"#;
    let g = build(&[
        ("src/commands/impact/mod.rs", IMPACT_MOD),
        ("src/commands/walk/mod.rs", WALK_MOD),
        ("src/commands/impact/symbol.rs", symbol_rs),
    ]);
    assert_eq!(
        callee_files_of_run(&g),
        vec!["src/commands/impact/mod.rs".to_string()]
    );
}

/// File paths of every `Item::make`/`helper` target that `caller` calls.
fn callee_files(graph: &ZeroCopyGraph, caller: &str, callee: &str) -> Vec<String> {
    let pool = graph.string_pool.as_slice();
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == callee)
        .map(|e| {
            let file_idx = graph.nodes[e.target as usize].file_idx as usize;
            graph.files[file_idx].path.resolve(pool).to_string()
        })
        .collect()
}

/// A crate at the repo root has repo-relative paths that start at `src/`,
/// with no `/src/` segment to anchor `crate::` on.
#[test]
fn test_crate_import_from_repo_root_crate_resolves_to_imported_module() {
    let g = build(&[
        ("src/lib.rs", "mod a;\nmod b;\nmod app;\n"),
        ("src/a.rs", "pub fn emit() {}\n"),
        ("src/b.rs", "pub fn emit() {}\n"),
        (
            "src/app.rs",
            "use crate::a::emit;\npub fn run() { emit(); }\n",
        ),
    ]);
    assert_eq!(
        callee_files(&g, "run", "emit"),
        vec!["src/a.rs".to_string()],
        "`use crate::a::emit` from src/app.rs is src/a.rs, not the same-named src/b.rs"
    );
}

/// `use crate::error as err;` makes `err::Error` name `crate::error::Error`:
/// the module alias expands before the prefix is compared with the import.
#[test]
fn test_path_qualifier_through_module_alias_matches_import() {
    let error_rs = "pub struct Error;\nimpl Error {\n    pub fn custom() -> Error { Error }\n}\n";
    let app_rs = "use crate::error as err;\nuse crate::error::Error;\n\
        pub fn aliased() { err::Error::custom(); }\n";
    let g = build(&[
        ("crates/x/src/lib.rs", "mod error;\nmod other;\nmod app;\n"),
        ("crates/x/src/error.rs", error_rs),
        ("crates/x/src/other.rs", error_rs),
        ("crates/x/src/app.rs", app_rs),
    ]);
    assert_eq!(
        callee_files(&g, "aliased", "custom"),
        vec!["crates/x/src/error.rs".to_string()]
    );
}

/// `use crate::X` names an item of the crate root module itself: `src/lib.rs`
/// for a library, `src/main.rs` for a binary.
#[test]
fn test_crate_root_item_import_resolves_to_root_file() {
    for root_file in ["src/lib.rs", "src/main.rs"] {
        let g = build(&[
            (root_file, "mod app;\nmod other;\npub fn emit() {}\n"),
            ("src/other.rs", "pub fn emit() {}\n"),
            ("src/app.rs", "use crate::emit;\npub fn run() { emit(); }\n"),
        ]);
        assert_eq!(callee_files(&g, "run", "emit"), vec![root_file.to_string()]);
    }
}

/// A Rust module path names a Rust file: a same-stem TypeScript file next
/// to the module must not win the probe.
#[test]
fn test_crate_import_ignores_same_stem_file_of_another_language() {
    for root in ["", "crates/x/"] {
        let p = |f: &str| format!("{root}{f}");
        let g = build_with_ts(
            &[
                (p("src/lib.rs"), "mod a;\nmod app;\n"),
                (p("src/a.rs"), "pub fn emit() {}\n"),
                (
                    p("src/app.rs"),
                    "use crate::a::emit;\npub fn run() { emit(); }\n",
                ),
            ],
            &p("src/a.ts"),
            "export function emit() {}\n",
        );
        assert_eq!(
            callee_files(&g, "run", "emit"),
            vec![p("src/a.rs")],
            "root {root:?}"
        );
    }
}

/// `de::Error::custom` names `Error` through the module `de` (here the
/// external `serde::de`); an import of a different `Error` from
/// `crate::error` is not that qualifier. A second project `Error` keeps the
/// global lookup ambiguous, as in serde_json.
#[test]
fn test_path_qualifier_whose_module_prefix_differs_from_import_is_not_bound() {
    let error_rs = "pub struct Error;\nimpl Error {\n    pub fn custom() -> Error { Error }\n}\n";
    let app_rs = "use crate::error::Error;\nuse serde::de;\n\
        pub fn generic() { de::Error::custom(); }\n\
        pub fn direct() { Error::custom(); }\n\
        pub fn spelled() { crate::error::Error::custom(); }\n";
    let g = build(&[
        ("src/lib.rs", "mod error;\nmod other;\nmod app;\n"),
        ("src/error.rs", error_rs),
        ("src/other.rs", error_rs),
        ("src/app.rs", app_rs),
    ]);
    assert!(callee_files(&g, "generic", "custom").is_empty());
    assert_eq!(
        callee_files(&g, "direct", "custom"),
        vec!["src/error.rs".to_string()]
    );
    assert_eq!(
        callee_files(&g, "spelled", "custom"),
        vec!["src/error.rs".to_string()]
    );
}

#[test]
fn test_self_import_from_non_mod_file_resolves_to_stem_directory_child() {
    let item = "pub struct Item;\nimpl Item { pub fn make() -> Item { Item } }\n";
    let b_rs = "use self::c::Item;\nfn run() { Item::make(); }\n";
    let g = build(&[
        ("src/a/b.rs", b_rs),
        ("src/a/b/c.rs", item),
        ("src/a/c.rs", item),
    ]);
    assert_eq!(
        callee_files(&g, "run", "make"),
        vec!["src/a/b/c.rs".to_string()],
        "`self::c` from src/a/b.rs is src/a/b/c.rs, not the sibling src/a/c.rs"
    );
}

#[test]
fn test_self_import_from_cargo_test_root_resolves_to_sibling_module() {
    let it_rs = "mod support;\nuse self::support::helper;\nfn t() { helper(); }\n";
    let g = build(&[
        ("tests/it.rs", it_rs),
        ("tests/support.rs", "pub fn helper() {}\n"),
        ("src/other.rs", "pub fn helper() {}\n"),
    ]);
    assert_eq!(
        callee_files(&g, "t", "helper"),
        vec!["tests/support.rs".to_string()],
        "tests/it.rs is a crate root: `self::support` is tests/support.rs"
    );
}

#[test]
fn test_self_import_from_custom_lib_path_root_falls_back_to_own_directory() {
    // `[lib] path = "src/api.rs"`: the root is not named lib.rs, so the
    // module's own child directory (src/api/) is probed first and is empty.
    let api_rs = "mod child;\nuse self::child::helper;\npub fn run() { helper(); }\n";
    let g = build(&[
        ("src/api.rs", api_rs),
        ("src/child.rs", "pub fn helper() {}\n"),
        ("src/other/child.rs", "pub fn helper() {}\n"),
    ]);
    assert_eq!(
        callee_files(&g, "run", "helper"),
        vec!["src/child.rs".to_string()],
        "with no src/api/child.rs, `self::child` falls back to src/child.rs"
    );
}

const DECOY_RS: &str =
    "pub struct Decoy;\n\nimpl Decoy {\n    pub fn greet(&self) -> u8 {\n        2\n    }\n}\n";

const APP_RS: &str =
    "use crate::model::Derived;\n\nfn run(d: &Derived) -> u8 {\n    d.greet()\n}\n";

/// Rust has no class inheritance; its inherited member is a trait default
/// method reached through `impl Greeter for Derived`. The trait-impl method
/// carries the heritage (attributed to `Derived` by the supertypes pre-pass),
/// and the trait-body method's owner is the trait. A decoy `greet` keeps the
/// bare name ambiguous.
#[test]
fn test_typed_receiver_trait_default_method_resolves_to_trait() {
    let greeter = "pub trait Greeter {\n    fn greet(&self) -> u8 {\n        1\n    }\n}\n";
    let model = "use crate::greeter::Greeter;\n\npub struct Derived;\n\nimpl Greeter for Derived {\n    fn id(&self) -> u8 {\n        0\n    }\n}\n";
    let g = build(&[
        ("src/greeter.rs", greeter),
        ("src/model.rs", model),
        ("src/decoy.rs", DECOY_RS),
        ("src/app.rs", APP_RS),
    ]);
    assert_eq!(
        callee_files(&g, "run", "greet"),
        vec!["src/greeter.rs".to_string()],
        "`Derived.greet` is Greeter's default method, once"
    );
}

/// An inherent method in an `impl` block in another file of the crate: the
/// type's file declares no `greet`, so only the owner index finds it.
#[test]
fn test_typed_receiver_method_in_sibling_impl_file_resolves_to_impl() {
    let model = "pub struct Derived;\n";
    let model_impl = "use crate::model::Derived;\n\nimpl Derived {\n    pub fn greet(&self) -> u8 {\n        1\n    }\n}\n";
    let g = build(&[
        ("src/model.rs", model),
        ("src/model_impl.rs", model_impl),
        ("src/decoy.rs", DECOY_RS),
        ("src/app.rs", APP_RS),
    ]);
    assert_eq!(
        callee_files(&g, "run", "greet"),
        vec!["src/model_impl.rs".to_string()]
    );
}

const MODEL_IMPL_RS: &str =
    "pub struct Derived;\n\nimpl Derived {\n    pub fn greet(&self) -> u8 {\n        1\n    }\n}\n";

/// A Rust type name reaches a file only through a `use` or its own
/// definition. With neither on record (here the parser drops the nested
/// `io::Derived` of an external crate), the project type of that name is a
/// guess: tokio's `use tokio_test::{io::Builder}` bound to its own Builder.
/// A second project `Derived` keeps the earlier tiers ambiguous, as tokio's
/// several `Builder` types do.
#[test]
fn test_typed_receiver_with_unrecorded_type_source_gets_no_edge() {
    let app =
        "use ext::{assert_ok, io::Derived};\n\nfn run(d: &Derived) -> u8 {\n    d.greet()\n}\n";
    let g = build(&[
        ("src/model.rs", MODEL_IMPL_RS),
        ("src/other.rs", "pub struct Derived;\n"),
        ("src/decoy.rs", DECOY_RS),
        ("src/app.rs", app),
    ]);
    assert!(callee_files(&g, "run", "greet").is_empty());
}

/// A generic parameter is not a project type, even when a project type or
/// a blanket impl's self type shares its one-letter name.
#[test]
fn test_typed_receiver_generic_parameter_gets_no_edge() {
    let wait = "pub trait Wait {\n    fn id(&self) -> u32;\n}\n\nimpl<T: Wait> Wait for &mut T {\n    fn id(&self) -> u32 {\n        0\n    }\n}\n";
    let alias = "pub type T = u8;\n";
    let app = "fn run<T: crate::wait::Wait>(future: T) -> u32 {\n    future.id()\n}\n";
    let g = build(&[
        ("src/wait.rs", wait),
        ("src/alias.rs", alias),
        ("src/app.rs", app),
    ]);
    assert!(callee_files(&g, "run", "id").is_empty());
}

/// `use crate::model::Derived` through a module that re-exports it
/// (`pub use`): the import is on record and in the project, so the type's
/// global candidates still apply.
#[test]
fn test_typed_receiver_through_reexporting_module_import_resolves() {
    let app = "use crate::model::Derived;\n\nfn run(d: &Derived) -> u8 {\n    d.greet()\n}\n";
    let g = build(&[
        ("src/model/mod.rs", "mod inner;\npub use inner::Derived;\n"),
        ("src/model/inner.rs", MODEL_IMPL_RS),
        ("src/decoy.rs", DECOY_RS),
        ("src/app.rs", app),
    ]);
    assert_eq!(
        callee_files(&g, "run", "greet"),
        vec!["src/model/inner.rs".to_string()]
    );
}

/// `(file, owner)` of every `callee` node that `caller` calls, sorted.
fn callee_owners(graph: &ZeroCopyGraph, caller: &str, callee: &str) -> Vec<(String, String)> {
    let pool = graph.string_pool.as_slice();
    let mut owners: Vec<(String, String)> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == callee)
        .map(|e| {
            let target = &graph.nodes[e.target as usize];
            let file_idx = target.file_idx as usize;
            (
                graph.files[file_idx].path.resolve(pool).to_string(),
                target.owner_class.resolve(pool).to_string(),
            )
        })
        .collect();
    owners.sort();
    owners
}

/// Each `Self::` caller beside the same call spelled with the type's name.
/// `U` declares a second `dup_helper` and `new`, so only the caller's own
/// impl type tells the two apart.
const SELF_PATHS_RS: &str = r#"pub struct S;
impl S {
    pub fn new() -> S { S }
    pub fn dup_helper() -> i32 { 1 }
    pub fn uniq_helper() -> i32 { 1 }
    pub fn self_dup() -> i32 { Self::dup_helper() }
    pub fn type_dup() -> i32 { S::dup_helper() }
    pub fn self_uniq() -> i32 { Self::uniq_helper() }
    pub fn self_closure() -> i32 { let f = || Self::dup_helper(); f() }
    pub fn self_as_trait() -> i32 { <Self as T>::thelper() }
}
pub struct U;
impl U {
    pub fn new() -> U { U }
    pub fn dup_helper() -> i32 { 2 }
}
pub trait T {
    fn make() -> Self;
    fn tdef_self() -> i32 { Self::thelper() }
    fn tdef_type() -> i32 { T::thelper() }
    fn thelper() -> i32;
}
impl T for S {
    fn make() -> Self { Self::new() }
    fn thelper() -> i32 { 2 }
}
impl T for U {
    fn make() -> Self { U::new() }
    fn thelper() -> i32 { 3 }
}
pub enum E { A(i32), B }
impl E {
    pub fn self_variant() -> E { Self::A(1) }
}
pub struct G<X>(X);
impl<X> G<X> {
    pub fn gnew() -> i32 { 0 }
    pub fn self_g() -> i32 { Self::gnew() }
}
pub fn free_self() -> i32 { Self::dup_helper() }
"#;

/// Rust `Self::m()` names the caller's own impl type, as `S::m()` does.
#[test]
fn test_rust_self_path_call_binds_caller_impl_type_member() {
    let g = build(&[("src/lib.rs", SELF_PATHS_RS)]);
    let s = vec![("src/lib.rs".to_string(), "S".to_string())];
    assert_eq!(callee_owners(&g, "type_dup", "dup_helper"), s, "control");
    assert_eq!(
        callee_owners(&g, "self_dup", "dup_helper"),
        s,
        "inherent impl"
    );
    assert_eq!(
        callee_owners(&g, "self_uniq", "uniq_helper"),
        s,
        "unique name"
    );
    assert_eq!(
        callee_owners(&g, "self_closure", "dup_helper"),
        s,
        "inside a closure"
    );
    assert_eq!(
        callee_owners(&g, "make", "new"),
        vec![
            ("src/lib.rs".to_string(), "S".to_string()),
            ("src/lib.rs".to_string(), "U".to_string()),
        ],
        "`impl T for S` reaches the inherent `S::new`; `U::new` is the control"
    );
    assert_eq!(
        callee_owners(&g, "self_g", "gnew"),
        vec![("src/lib.rs".to_string(), "G".to_string())],
        "generic impl"
    );
}

/// In a trait's default method `Self::m()` binds the trait's own declaration,
/// as `T::m()` does.
#[test]
fn test_rust_self_path_call_in_trait_default_binds_trait_decl() {
    let g = build(&[("src/lib.rs", SELF_PATHS_RS)]);
    let t = vec![("src/lib.rs".to_string(), "T".to_string())];
    assert_eq!(callee_owners(&g, "tdef_type", "thelper"), t, "control");
    assert_eq!(callee_owners(&g, "tdef_self", "thelper"), t);
}

/// `Self::A(..)` stays a variant reference, never a call; a free function
/// has no `Self` and binds nothing; `<Self as T>::m()` is no `Self` path and
/// binds no impl's member.
#[test]
fn test_rust_self_path_without_impl_owner_or_as_trait_gets_no_impl_edge() {
    let g = build(&[("src/lib.rs", SELF_PATHS_RS)]);
    assert!(callee_owners(&g, "self_variant", "A").is_empty());
    let pool = g.string_pool.as_slice();
    let variant_refs = g
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::References)
        .filter(|e| g.nodes[e.source as usize].name.resolve(pool) == "self_variant")
        .filter(|e| g.nodes[e.target as usize].name.resolve(pool) == "A")
        .count();
    assert_eq!(variant_refs, 1, "`Self::A(1)` references the variant once");
    assert!(callee_owners(&g, "free_self", "dup_helper").is_empty());
    assert!(
        callee_owners(&g, "self_as_trait", "thelper")
            .iter()
            .all(|(_, owner)| owner == "T"),
        "{:?}",
        callee_owners(&g, "self_as_trait", "thelper")
    );
}

/// `tonly` is only a default method of trait `T`; `U` declares an inherent
/// `tonly` of its own.
const SELF_TRAIT_DEFAULT_RS: &str = r#"pub trait T {
    fn tonly() -> i32 { 1 }
    fn s_self() -> i32 { 0 }
    fn s_type() -> i32 { 0 }
    fn u_self() -> i32 { 0 }
}
pub struct S;
impl T for S {
    fn s_self() -> i32 { Self::tonly() }
    fn s_type() -> i32 { S::tonly() }
}
pub struct U;
impl U {
    pub fn tonly() -> i32 { 2 }
}
impl T for U {
    fn u_self() -> i32 { Self::tonly() }
}
"#;

/// In `impl T for S`, `Self::tonly()` reaches the trait's default method
/// through the receiver-typing ladder, as `S::tonly()` does; in `impl T for
/// U` it binds `U`'s inherent `tonly`.
#[test]
fn test_rust_self_path_call_trait_default_only_binds_through_heritage() {
    let g = build(&[("src/lib.rs", SELF_TRAIT_DEFAULT_RS)]);
    let t = vec![("src/lib.rs".to_string(), "T".to_string())];
    assert_eq!(callee_owners(&g, "s_type", "tonly"), t, "control");
    assert_eq!(callee_owners(&g, "s_self", "tonly"), t);
    assert_eq!(
        callee_owners(&g, "u_self", "tonly"),
        vec![("src/lib.rs".to_string(), "U".to_string())]
    );
}

/// `rkyv::Archive` generates `ArchivedZg`: the project declares it only
/// through `impl ArchivedZg`, so no type node carries the name. `other.rs`
/// holds a free `fn later` a bare-name tier could guess.
const ARCHIVED_GRAPH_RS: &str = r#"#[derive(rkyv::Archive)]
pub struct Zg;
impl ArchivedZg {
    pub fn later(&self, i: u32) -> u32 { i }
    pub fn uniq_arch(&self) -> u32 { 0 }
}
pub fn same_file(g: &ArchivedZg) -> u32 { g.later(1) }
pub fn same_file_uniq(g: &ArchivedZg) -> u32 { g.uniq_arch() }
"#;

fn archived_crate(graph_rs: &str) -> ZeroCopyGraph {
    build(&[
        ("src/lib.rs", "pub mod graph;\npub mod other;\npub mod user;\n"),
        ("src/graph.rs", graph_rs),
        ("src/other.rs", "pub fn later(i: u32) -> u32 { i }\n"),
        (
            "src/user.rs",
            "use crate::graph::ArchivedZg;\npub fn use_arch(g: &ArchivedZg) -> u32 { g.later(1) }\n",
        ),
    ])
}

/// A method-syntax call on a receiver typed as an impl-only type binds the
/// `impl` block's method, from the impl's own file and through a `use`;
/// the same-named free `fn later` never binds.
#[test]
fn test_rust_typed_receiver_of_impl_only_type_binds_the_impl_method() {
    let g = archived_crate(ARCHIVED_GRAPH_RS);
    let arch = vec![("src/graph.rs".to_string(), "ArchivedZg".to_string())];
    assert_eq!(callee_owners(&g, "same_file", "later"), arch, "same_file");
    assert_eq!(
        callee_owners(&g, "same_file_uniq", "uniq_arch"),
        arch,
        "same_file_uniq"
    );
    assert_eq!(callee_owners(&g, "use_arch", "later"), arch, "use_arch");
}

/// `impl Debug for ArchivedZg` beside `impl ArchivedZg` is one more block in
/// the same file: the qualifier still names one file.
#[test]
fn test_rust_typed_receiver_of_impl_only_type_two_blocks_one_file_binds() {
    let graph_rs = format!(
        "{ARCHIVED_GRAPH_RS}impl std::fmt::Debug for ArchivedZg {{\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{ Ok(()) }}\n}}\n"
    );
    let g = archived_crate(&graph_rs);
    assert_eq!(
        callee_owners(&g, "use_arch", "later"),
        vec![("src/graph.rs".to_string(), "ArchivedZg".to_string())]
    );
}

/// `impl ArchivedZg` blocks in two files: the caller in a block's own file
/// binds that file's method. A caller in neither file has no one qualifier
/// file today, so `use_arch` misses its edge; the contract is only that it
/// never binds a wrong one.
#[test]
fn test_rust_typed_receiver_of_impl_only_type_in_two_files_never_binds_a_wrong_file() {
    let g = build(&[
        (
            "src/lib.rs",
            "pub mod graph;\npub mod graph_ext;\npub mod user;\n",
        ),
        ("src/graph.rs", ARCHIVED_GRAPH_RS),
        (
            "src/graph_ext.rs",
            "use crate::graph::ArchivedZg;\nimpl ArchivedZg {\n    pub fn extra(&self) -> u32 { 2 }\n}\n",
        ),
        (
            "src/user.rs",
            "use crate::graph::ArchivedZg;\npub fn use_arch(g: &ArchivedZg) -> u32 { g.later(1) }\n",
        ),
    ]);
    assert_eq!(
        callee_owners(&g, "same_file", "later"),
        vec![("src/graph.rs".to_string(), "ArchivedZg".to_string())]
    );
    let use_arch = callee_owners(&g, "use_arch", "later");
    assert!(
        use_arch
            .iter()
            .all(|(file, owner)| file == "src/graph.rs" && owner == "ArchivedZg"),
        "{use_arch:?}"
    );
}

/// `impl From<Local> for String` is the project's only `String`, and
/// `String::from("x")` is a path that calls std: it binds nothing. A
/// method-syntax `s.len()` finds no `len` in that impl and binds nothing
/// either.
#[test]
fn test_rust_path_call_on_impl_only_std_type_binds_nothing() {
    let g = build(&[(
        "src/lib.rs",
        "pub struct Local;\nimpl From<Local> for String {\n    fn from(_: Local) -> String { String::new() }\n}\npub fn f() -> String { String::from(\"x\") }\npub fn n(s: &String) -> usize { s.len() }\n",
    )]);
    assert!(callee_files(&g, "f", "from").is_empty());
    assert!(callee_files(&g, "n", "len").is_empty());
}

/// A type with both `struct S` and `impl S` stays one qualifier: the struct
/// names it, the impl block does not make it ambiguous. `T` declares a
/// second `m` a bare-name pick could take.
#[test]
fn test_rust_typed_receiver_of_struct_with_impl_binds_its_method() {
    let g = build(&[
        ("src/lib.rs", "pub mod s;\npub mod t;\npub mod user;\n"),
        (
            "src/s.rs",
            "pub struct S;\nimpl S {\n    pub fn m(&self) -> u32 { 0 }\n}\n",
        ),
        (
            "src/t.rs",
            "pub struct T;\nimpl T {\n    pub fn m(&self) -> u32 { 1 }\n}\n",
        ),
        (
            "src/user.rs",
            "use crate::s::S;\npub fn typed(x: &S) -> u32 { x.m() }\n",
        ),
    ]);
    assert_eq!(
        callee_owners(&g, "typed", "m"),
        vec![("src/s.rs".to_string(), "S".to_string())]
    );
}
