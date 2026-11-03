//! Rust calls the parser finds in two places a `call_expression` walk alone
//! misses or misplaces:
//!
//! | Shape | Before the fix | Contract |
//! |---|---|---|
//! | `super::f()` / `self::g()` in an inline `mod` | no edge, or an edge into the parent file's namesake | `Calls` into the item the path names |
//! | `f()` / `x.m()` / `a::f()` in macro arguments | `References` (`function_reference`) | one `Calls`, no `References` for the same site |
//! | `Pool::<u8>::f()` / `::f()` / `<Foo as Tr>::f()`, in or out of a macro | a bare-name guess, or no edge | the callee a plain path call spells: `Pool::f`, `f`, `Tr::f` |
//! | `super::View {}` in an inline `mod` | an edge into the parent file's namesake | `Calls` into the struct the path names |
//! | `use super::x;` / `use self::x;` / `use super::*;` in an inline `mod` | read from the file's module | the item the path names |
//!
//! Every fixture is a real crate on disk (a `Cargo.toml` beside `src/`), so
//! the module tree that places `self::` / `super::` paths is built. Every
//! assertion reads the graph the parse + `GraphBuilder` pipeline built.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawImport};
use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind, RelType, ZeroCopyGraph};
use ecp_core::session::{MergedGraph, OverlayFileInput, OverlayView};
use std::path::Path;

const MANIFEST: &str = "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

/// One edge out of a caller: `(target file, target name, target line)`,
/// the line 1-based.
type Hit = (String, String, u32);

fn parse(path: &str, src: &str) -> LocalGraph {
    RustProvider::new()
        .expect("RustProvider::new")
        .parse_file(Path::new(path), src.as_bytes())
        .expect("parse_file")
}

/// The full build of `files`, written under `root` with a Cargo manifest.
fn build_in(root: &Path, files: &[(&str, &str)]) -> ZeroCopyGraph {
    std::fs::write(root.join("Cargo.toml"), MANIFEST).expect("write manifest");
    let mut builder = GraphBuilder::new().with_repo_root(root.to_path_buf());
    for (rel, src) in files {
        let disk = root.join(rel);
        std::fs::create_dir_all(disk.parent().expect("parent")).expect("mkdir");
        std::fs::write(&disk, src).expect("write source");
        builder.add_graph(parse(rel, src));
    }
    builder.build()
}

fn build(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let tmp = tempfile::tempdir().expect("tempdir");
    build_in(tmp.path(), files)
}

fn build_lib(src: &str) -> ZeroCopyGraph {
    build(&[("src/lib.rs", src)])
}

fn is_callable(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Function | NodeKind::Method)
}

/// Every `rel` edge out of the callable `caller`, with `reason` when given,
/// sorted.
fn hits(g: &ZeroCopyGraph, caller: &str, rel: RelType, reason: Option<&str>) -> Vec<Hit> {
    let pool = g.string_pool.as_slice();
    let mut out: Vec<Hit> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == rel)
        .filter(|e| reason.is_none_or(|r| e.reason.resolve(pool) == r))
        .filter(|e| {
            let source = &g.nodes[e.source as usize];
            is_callable(source.kind) && source.name.resolve(pool) == caller
        })
        .map(|e| {
            let target = &g.nodes[e.target as usize];
            let file = if target.has_owning_file() {
                g.files[target.file_idx as usize]
                    .path
                    .resolve(pool)
                    .to_string()
            } else {
                String::new()
            };
            (
                file,
                target.name.resolve(pool).to_string(),
                target.span.0 + 1,
            )
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn calls(g: &ZeroCopyGraph, caller: &str) -> Vec<Hit> {
    hits(g, caller, RelType::Calls, None)
}

/// The names `caller` reaches by a `References` edge of any reason.
fn referenced(g: &ZeroCopyGraph, caller: &str) -> Vec<String> {
    hits(g, caller, RelType::References, None)
        .into_iter()
        .map(|(_, name, _)| name)
        .collect()
}

fn lib(name: &str, line: u32) -> Hit {
    ("src/lib.rs".to_string(), name.to_string(), line)
}

// ── `self::` / `super::` from an inline `mod` ────────────────────────────────

#[test]
fn test_build_rust_super_path_in_inline_mod_calls_top_level_fn() {
    let g = build_lib(
        "pub fn helper() -> i32 { 1 }\n\
         mod inner {\n\
         \x20   pub fn run() -> i32 { super::helper() }\n\
         }\n",
    );
    assert_eq!(calls(&g, "run"), vec![lib("helper", 1)]);
}

#[test]
fn test_build_rust_self_path_in_inline_mod_calls_mod_item() {
    let g = build_lib(
        "pub fn g() -> i32 { 0 }\n\
         mod inner {\n\
         \x20   pub fn g() -> i32 { 1 }\n\
         \x20   pub fn run() -> i32 { self::g() }\n\
         }\n",
    );
    assert_eq!(
        calls(&g, "run"),
        vec![lib("g", 3)],
        "`self` inside `mod inner` is `inner`, not the file's module"
    );
}

#[test]
fn test_build_rust_super_chain_in_nested_inline_mods_pops_one_mod_per_super() {
    let g = build_lib(
        "pub fn f() -> i32 { 0 }\n\
         mod a {\n\
         \x20   pub fn f() -> i32 { 1 }\n\
         \x20   mod b {\n\
         \x20       pub fn up_two() -> i32 { super::super::f() }\n\
         \x20       pub fn up_one() -> i32 { super::f() }\n\
         \x20   }\n\
         }\n",
    );
    assert_eq!(
        calls(&g, "up_two"),
        vec![lib("f", 1)],
        "super::super from a::b is the crate root"
    );
    assert_eq!(
        calls(&g, "up_one"),
        vec![lib("f", 3)],
        "super from a::b is a"
    );
}

/// A file module's own `super` keeps its meaning; an inline `mod` inside
/// it climbs to the file module first, and a longer chain leaves the file.
#[test]
fn test_build_rust_super_path_in_file_module_and_its_inline_mod_binds_each_parent() {
    let g = build(&[
        ("src/lib.rs", "pub mod a;\npub fn f() -> i32 { 0 }\n"),
        (
            "src/a.rs",
            "pub fn f() -> i32 { 1 }\n\
             pub fn top_a() -> i32 { super::f() }\n\
             mod inner {\n\
             \x20   pub fn run_a() -> i32 { super::f() }\n\
             \x20   pub fn run_a2() -> i32 { super::super::f() }\n\
             }\n",
        ),
    ]);
    assert_eq!(
        calls(&g, "top_a"),
        vec![lib("f", 2)],
        "existing: a.rs's super is the crate root"
    );
    assert_eq!(
        calls(&g, "run_a"),
        vec![("src/a.rs".to_string(), "f".to_string(), 1)],
        "super from a::inner is a, never lib.rs's namesake"
    );
    assert_eq!(calls(&g, "run_a2"), vec![lib("f", 2)]);
}

#[test]
fn test_build_rust_same_name_in_inline_mod_and_top_level_binds_by_path() {
    let g = build_lib(
        "pub fn f() -> i32 { 0 }\n\
         mod inner {\n\
         \x20   pub fn f() -> i32 { 1 }\n\
         \x20   pub fn via_super() -> i32 { super::f() }\n\
         \x20   pub fn bare() -> i32 { f() }\n\
         }\n",
    );
    assert_eq!(calls(&g, "via_super"), vec![lib("f", 1)]);
    assert_eq!(
        calls(&g, "bare"),
        vec![lib("f", 3)],
        "a bare call binds the local f"
    );
}

// ── calls in macro arguments ─────────────────────────────────────────────────

const MACRO_LIB: &str = "pub fn f() -> i32 { 0 }\n\
pub fn g() -> i32 { 3 }\n\
pub struct W;\n\
impl W {\n\
\x20   pub fn m(&self) -> i32 { 1 }\n\
\x20   pub fn via_self(&self) { println!(\"{}\", self.m()); }\n\
}\n\
pub fn in_println() { println!(\"{}\", f()); }\n\
pub fn in_vec() -> Vec<i32> { vec![f()] }\n\
pub fn in_assert() { assert_eq!(g(), 3); }\n\
pub fn in_format(x: &W) -> String { format!(\"{}\", x.m()) }\n\
pub fn in_nested() -> Vec<String> { vec![format!(\"{}\", f())] }\n\
pub fn two_calls() { assert_eq!(f(), g()); }\n\
pub fn commented() { println!(\"{}\", f /* c */ ()); }\n\
pub fn by_path() { println!(\"{}\", crate::g()); }\n";

/// Each macro-argument call is one `Calls` edge and no `References` edge.
fn assert_macro_calls(caller: &str, expected: &[Hit]) {
    let g = build_lib(MACRO_LIB);
    assert_eq!(calls(&g, caller), expected, "{caller}: Calls");
    let names: Vec<&str> = expected.iter().map(|(_, name, _)| name.as_str()).collect();
    let duplicated: Vec<String> = referenced(&g, caller)
        .into_iter()
        .filter(|name| names.contains(&name.as_str()))
        .collect();
    assert!(
        duplicated.is_empty(),
        "{caller}: a References edge duplicates the call: {duplicated:?}"
    );
}

#[test]
fn test_build_rust_println_argument_is_call() {
    assert_macro_calls("in_println", &[lib("f", 1)]);
}

#[test]
fn test_build_rust_vec_element_is_call() {
    assert_macro_calls("in_vec", &[lib("f", 1)]);
}

#[test]
fn test_build_rust_assert_eq_argument_is_call() {
    assert_macro_calls("in_assert", &[lib("g", 2)]);
}

#[test]
fn test_build_rust_format_member_call_on_typed_param_binds_method() {
    assert_macro_calls("in_format", &[lib("m", 5)]);
}

#[test]
fn test_build_rust_self_member_call_in_macro_binds_own_method() {
    assert_macro_calls("via_self", &[lib("m", 5)]);
}

#[test]
fn test_build_rust_nested_macro_argument_is_call() {
    assert_macro_calls("in_nested", &[lib("f", 1)]);
}

#[test]
fn test_build_rust_two_calls_in_one_macro_are_both_calls() {
    assert_macro_calls("two_calls", &[lib("f", 1), lib("g", 2)]);
}

#[test]
fn test_build_rust_comment_between_name_and_arguments_is_call() {
    assert_macro_calls("commented", &[lib("f", 1)]);
}

#[test]
fn test_build_rust_path_call_in_macro_is_call() {
    assert_macro_calls("by_path", &[lib("g", 2)]);
}

/// The brief's repro: a `super::` call and macro-argument calls in an inline
/// `mod`, and macro-argument calls at the top level. The bug means
/// `run` has no edge and the macro sites are `References`; the fix means
/// five `Calls` and no `References` into these targets.
const REPRO: &str = "pub fn helper() -> i32 { 1 }\n\
pub fn other() -> i32 { 2 }\n\
mod inner {\n\
\x20   pub fn run() -> i32 { super::helper() }\n\
\x20   pub fn run_mac() {\n\
\x20       println!(\"{}\", super::other());\n\
\x20       assert_eq!(compute(), 3);\n\
\x20   }\n\
\x20   fn compute() -> i32 { 3 }\n\
}\n\
pub fn top() {\n\
\x20   println!(\"{}\", helper());\n\
\x20   let v = vec![other()];\n\
\x20   drop(v);\n\
}\n";

#[test]
fn test_build_rust_inline_mod_repro_gives_calls_and_no_references() {
    let g = build_lib(REPRO);
    assert_eq!(calls(&g, "run"), vec![lib("helper", 1)]);
    assert_eq!(
        calls(&g, "run_mac"),
        vec![lib("compute", 9), lib("other", 2)]
    );
    assert_eq!(calls(&g, "top"), vec![lib("helper", 1), lib("other", 2)]);
    for caller in ["run", "run_mac", "top"] {
        let stray: Vec<String> = referenced(&g, caller)
            .into_iter()
            .filter(|name| matches!(name.as_str(), "helper" | "other" | "compute"))
            .collect();
        assert!(stray.is_empty(), "{caller}: References {stray:?}");
    }
}

// ── token runs that are no call ──────────────────────────────────────────────

const NEGATIVE_LIB: &str = "pub fn f() -> i32 { 0 }\n\
pub fn g() -> i32 { 1 }\n\
macro_rules! top_m { () => { f() } }\n\
pub fn outer() -> i32 { macro_rules! m { () => { f() } } 1 }\n\
pub fn quoted() { let _t = quote!(fn f() {}); }\n\
pub fn interpolated() { let _t = quote!(#f()); }\n\
pub fn empty() { f!(); }\n\
pub fn tuple() { println!(\"{:?}\", (f, g)); }\n\
pub fn turbofish() { println!(\"{:?}\", Vec::<u8>::new()); }\n";

#[test]
fn test_build_rust_macro_rules_body_gives_no_call_from_enclosing_fn() {
    let g = build_lib(NEGATIVE_LIB);
    assert_eq!(calls(&g, "outer"), vec![]);
    // Contract: only a site the call walker records loses its name read, so
    // a `macro_rules!` body keeps the reference it had.
    assert!(
        referenced(&g, "outer").contains(&"f".to_string()),
        "{:?}",
        referenced(&g, "outer")
    );
}

#[test]
fn test_build_rust_quoted_fn_declaration_gives_no_call() {
    let g = build_lib(NEGATIVE_LIB);
    assert_eq!(calls(&g, "quoted"), vec![]);
}

#[test]
fn test_build_rust_quote_interpolation_gives_no_call() {
    let g = build_lib(NEGATIVE_LIB);
    assert_eq!(calls(&g, "interpolated"), vec![]);
}

#[test]
fn test_build_rust_empty_macro_arguments_give_no_call() {
    let g = build_lib(NEGATIVE_LIB);
    assert_eq!(calls(&g, "empty"), vec![]);
}

#[test]
fn test_build_rust_tuple_after_comma_is_no_call_and_keeps_references() {
    let g = build_lib(NEGATIVE_LIB);
    assert_eq!(calls(&g, "tuple"), vec![]);
    let names = referenced(&g, "tuple");
    assert!(
        ["f", "g"].iter().all(|n| names.iter().any(|r| r == n)),
        "a function named as a value stays a reference: {names:?}"
    );
}

#[test]
fn test_build_rust_turbofish_path_in_macro_gives_no_bare_call() {
    let g = build_lib(&format!("{NEGATIVE_LIB}pub fn new() -> u8 {{ 0 }}\n"));
    assert_eq!(
        calls(&g, "turbofish"),
        vec![],
        "`Vec::<u8>::new` is `Vec::new`, as outside a macro: never the bare `new`"
    );
}

// ── path forms: the callee a macro argument spells is the one a plain call spells

const PATH_FORMS_LIB: &str = "pub fn f() -> i32 { 0 }\n\
pub struct Pool;\n\
impl Pool {\n\
\x20   pub fn with_slots(n: usize) -> usize { n }\n\
}\n\
pub trait Tr { fn make() -> Self; }\n\
pub struct Bar;\n\
impl Tr for Bar { fn make() -> Self { Bar } }\n\
pub struct Foo;\n\
impl Tr for Foo { fn make() -> Self { Foo } }\n\
pub fn rooted_in_assert() { assert!(::f() == 0); }\n\
pub fn turbofish_in_println() { println!(\"{}\", Pool::<u8>::with_slots(4)); }\n\
pub fn qualified_in_println() { println!(\"{:?}\", <Foo as Tr>::make()); }\n\
pub fn rooted() -> i32 { ::f() }\n\
pub fn turbofish() -> usize { Pool::<u8>::with_slots(4) }\n\
pub fn qualified() -> Foo { <Foo as Tr>::make() }\n";

/// `caller` has the one `Calls` edge `expected`, and no `References` edge
/// into the same name.
fn assert_one_call(g: &ZeroCopyGraph, caller: &str, expected: Hit) {
    assert_eq!(calls(g, caller), vec![expected.clone()], "{caller}: Calls");
    assert!(
        !referenced(g, caller).contains(&expected.1),
        "{caller}: a References edge duplicates the call"
    );
}

/// The edition-2015 meaning of `::f`, the crate root's `f`; from 2018 a
/// root path names a crate, and `::a::f` binds as `a::f` does.
#[test]
fn test_build_rust_root_path_call_in_assert_binds_crate_fn() {
    let g = build_lib(PATH_FORMS_LIB);
    assert_one_call(&g, "rooted_in_assert", lib("f", 1));
}

#[test]
fn test_build_rust_turbofish_path_call_in_println_binds_method() {
    let g = build_lib(PATH_FORMS_LIB);
    assert_one_call(&g, "turbofish_in_println", lib("with_slots", 4));
}

/// `<Foo as Tr>::make()` binds the trait's `make` (line 6), as
/// `<Self as T>::m()` does: never one impl's `make`, `Bar`'s or `Foo`'s.
#[test]
fn test_build_rust_qualified_self_call_in_println_binds_trait_decl() {
    let g = build_lib(PATH_FORMS_LIB);
    assert_one_call(&g, "qualified_in_println", lib("make", 6));
}

/// The macro tests above hold only while a plain call binds the same way.
#[test]
fn test_build_rust_path_forms_outside_macro_bind_as_inside() {
    let g = build_lib(PATH_FORMS_LIB);
    for (plain, in_macro) in [
        ("rooted", "rooted_in_assert"),
        ("turbofish", "turbofish_in_println"),
        ("qualified", "qualified_in_println"),
    ] {
        assert_eq!(calls(&g, plain), calls(&g, in_macro), "{plain}");
        assert_eq!(calls(&g, plain).len(), 1, "{plain}: one call");
    }
}

// ── initializers outside every `fn` ──────────────────────────────────────────

/// `(source name, edge kind)` of every edge into `target` but the file's
/// `Defines`, sorted.
fn edges_into(g: &ZeroCopyGraph, target: &str) -> Vec<(String, String)> {
    let pool = g.string_pool.as_slice();
    let mut out: Vec<(String, String)> = g
        .edges
        .iter()
        .filter(|e| e.rel_type != RelType::Defines)
        .filter(|e| g.nodes[e.target as usize].name.resolve(pool) == target)
        .map(|e| {
            let source = g.nodes[e.source as usize].name.resolve(pool);
            (source.to_string(), format!("{:?}", e.rel_type))
        })
        .collect();
    out.sort();
    out
}

/// A closure in a `static` initializer holds the call in either form: the
/// closure's `<anonymous>` node calls `default_rule`, and the macro form
/// gives the edges the plain form gives.
#[test]
fn test_build_rust_static_closure_macro_call_binds_from_closure_as_plain_call() {
    let with_macro = build_lib(
        "use std::sync::LazyLock;\n\
         pub fn default_rule() -> i32 { 0 }\n\
         pub static D: LazyLock<Vec<i32>> = LazyLock::new(|| vec![default_rule()]);\n",
    );
    let plain = build_lib(
        "use std::sync::LazyLock;\n\
         pub fn default_rule() -> i32 { 0 }\n\
         pub static D: LazyLock<Vec<i32>> = LazyLock::new(|| [default_rule()].to_vec());\n",
    );
    let expected = vec![("<anonymous:3:49>".to_string(), "Calls".to_string())];
    assert_eq!(edges_into(&plain, "default_rule"), expected, "plain");
    assert_eq!(edges_into(&with_macro, "default_rule"), expected, "macro");
}

/// A `const` initializer has no function to call from: the macro argument's
/// name keeps the `References` edge it had before macro calls bound.
#[test]
fn test_build_rust_const_initializer_macro_call_keeps_reference() {
    let g = build_lib(
        "macro_rules! m { ($e:expr) => { $e } }\n\
         pub const fn f() -> i32 { 1 }\n\
         pub const X: i32 = m!(f());\n",
    );
    assert_eq!(
        edges_into(&g, "f"),
        [("X".to_string(), "References".to_string())]
    );
}

// ── struct literals in an inline `mod` ───────────────────────────────────────

/// `other.rs` declares a `View` too: only the path places the literal.
#[test]
fn test_build_rust_super_struct_literal_in_inline_mod_calls_parent_struct() {
    let g = build(&[
        (
            "src/lib.rs",
            "pub mod other;\n\
             pub struct View;\n\
             mod inner {\n\
             \x20   fn mk() { let _ = super::View {}; }\n\
             }\n",
        ),
        ("src/other.rs", "pub struct View;\n"),
    ]);
    assert_eq!(calls(&g, "mk"), vec![lib("View", 2)]);
}

#[test]
fn test_build_rust_macro_rules_body_gives_no_call_anywhere() {
    let g = build_lib(NEGATIVE_LIB);
    let pool = g.string_pool.as_slice();
    let into_f: Vec<&str> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| g.nodes[e.target as usize].name.resolve(pool) == "f")
        .map(|e| g.nodes[e.source as usize].name.resolve(pool))
        .collect();
    assert_eq!(into_f, Vec::<&str>::new());
}

// ── the overlay agrees with the full build ───────────────────────────────────

type EdgeRow = (String, String, u32);

/// `(source name, target name, target line)` of every `rel` edge into
/// `targets`, sorted.
fn merged_rows(merged: &MergedGraph<'_>, rel: RelType, targets: &[&str]) -> Vec<EdgeRow> {
    let mut out: Vec<EdgeRow> = merged
        .all_edges()
        .filter(|e| e.rel_type() == rel)
        .filter_map(|e| {
            let (s, t) = (merged.node(e.source)?, merged.node(e.target)?);
            targets.contains(&t.name(merged)).then(|| {
                (
                    s.name(merged).to_string(),
                    t.name(merged).to_string(),
                    t.start_line(),
                )
            })
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The full build of `files`, and the overlay view of `dirty` (one of them,
/// unchanged) over it, agree on every `Calls` and `References` edge into
/// `targets`. Returns the full build's `Calls` rows.
fn assert_overlay_matches_full(
    files: &[(&str, &str)],
    dirty: &str,
    targets: &[&str],
) -> Vec<EdgeRow> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let full = build_in(tmp.path(), files);
    let src = files
        .iter()
        .find(|(path, _)| *path == dirty)
        .map(|(_, src)| *src)
        .expect("setup: the dirty file is one of the files");
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&full).expect("archive graph");
    let base =
        rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).expect("access graph");
    let view =
        OverlayView::build(base, &[OverlayFileInput::from(parse(dirty, src))]).expect("a view");
    let (indexed, overlaid) = (
        MergedGraph::new(base, None),
        MergedGraph::new(base, Some(&view)),
    );
    for rel in [RelType::Calls, RelType::References] {
        assert_eq!(
            merged_rows(&overlaid, rel, targets),
            merged_rows(&indexed, rel, targets),
            "{rel:?}: overlay vs full build"
        );
    }
    merged_rows(&indexed, RelType::Calls, targets)
}

#[test]
fn test_build_rust_inline_mod_repro_overlay_matches_full_build() {
    let calls = assert_overlay_matches_full(
        &[("src/lib.rs", REPRO)],
        "src/lib.rs",
        &["helper", "other", "compute"],
    );
    assert_eq!(calls.len(), 5, "setup: the full build holds the five calls");
}

// ── `use super::..` / `use self::..` in an inline `mod` ──────────────────────

const UTIL: &str = "pub fn g() -> i32 { 1 }\n";
const OTHER: &str = "pub fn g() -> i32 { 2 }\n";

/// `lib`, with `src/util.rs` and `src/other.rs` each declaring a `g`: only
/// an import that reaches `util` binds a bare `g()`.
fn build_with_util(lib: &str) -> ZeroCopyGraph {
    build(&[
        ("src/lib.rs", lib),
        ("src/util.rs", UTIL),
        ("src/other.rs", OTHER),
    ])
}

fn util_g() -> Hit {
    ("src/util.rs".to_string(), "g".to_string(), 1)
}

#[test]
fn test_build_rust_use_super_item_in_inline_mod_binds_top_level_fn() {
    let g = build_lib(
        "pub fn helper() -> i32 { 0 }\n\
         mod inner {\n\
         \x20   use super::helper;\n\
         \x20   pub fn run() -> i32 { helper() }\n\
         }\n",
    );
    assert_eq!(calls(&g, "run"), vec![lib("helper", 1)]);
}

#[test]
fn test_build_rust_use_super_module_item_in_inline_mod_binds_through_parent() {
    let g = build_with_util(
        "pub mod util;\n\
         pub mod other;\n\
         mod inner {\n\
         \x20   use super::util::g;\n\
         \x20   pub fn run() -> i32 { g() }\n\
         }\n",
    );
    assert_eq!(
        calls(&g, "run"),
        vec![util_g()],
        "super from inner is the crate root, whose util holds g"
    );
}

#[test]
fn test_build_rust_use_super_glob_in_inline_mod_imports_glob_module() {
    let g = build_with_util(
        "pub mod util;\n\
         pub mod other;\n\
         mod inner {\n\
         \x20   use super::util::*;\n\
         \x20   pub fn run() -> i32 { g() }\n\
         }\n",
    );
    // The glob's module is placed: `lib.rs` imports `util.rs`. A glob is no
    // evidence for the bare `g()` while `other.rs` declares a `g` too, with
    // or without the inline mod (`use crate::util::*;` at the top level of
    // `lib.rs` binds none either), so the call is not asserted here.
    assert_eq!(
        imports_from(&g, "src/lib.rs"),
        vec![Some("src/util.rs".to_string())]
    );
}

/// `use super::*` in a `mod tests` of the same file: the parent's own items
/// stay bound, in the file module and in its inline `mod`.
#[test]
fn test_build_rust_use_super_star_in_inline_mod_keeps_parent_items() {
    let g = build(&[
        (
            "src/lib.rs",
            "pub mod a;\n\
             pub mod other;\n\
             pub fn helper() -> i32 { 5 }\n\
             mod tests {\n\
             \x20   use super::*;\n\
             \x20   pub fn t() -> i32 { helper() }\n\
             }\n",
        ),
        (
            "src/a.rs",
            "pub fn f() -> i32 { 1 }\n\
             mod inner {\n\
             \x20   use super::*;\n\
             \x20   pub fn r() -> i32 { f() }\n\
             }\n",
        ),
        (
            "src/other.rs",
            "pub fn f() -> i32 { 2 }\npub fn helper() -> i32 { 6 }\n",
        ),
    ]);
    assert_eq!(calls(&g, "t"), vec![lib("helper", 3)]);
    assert_eq!(
        calls(&g, "r"),
        vec![("src/a.rs".to_string(), "f".to_string(), 1)]
    );
}

/// The import names an inline `mod` of the same file. No top-level `g`
/// stands beside it: with one, a bare `g()` binds the first `g` of the file
/// whatever the import says, before and after this rewrite.
#[test]
fn test_build_rust_use_self_path_in_inline_mod_binds_nested_mod_item() {
    let g = build_lib(
        "mod inner {\n\
         \x20   mod deep {\n\
         \x20       pub fn g() -> i32 { 7 }\n\
         \x20   }\n\
         \x20   use self::deep::g;\n\
         \x20   pub fn run() -> i32 { g() }\n\
         }\n",
    );
    assert_eq!(calls(&g, "run"), vec![lib("g", 3)]);
}

#[test]
fn test_build_rust_use_super_chain_in_nested_inline_mods_pops_one_mod_per_super() {
    let g = build_with_util(
        "pub mod util;\n\
         pub mod other;\n\
         mod a {\n\
         \x20   mod b {\n\
         \x20       use super::super::util::g;\n\
         \x20       pub fn run_b() -> i32 { g() }\n\
         \x20   }\n\
         }\n",
    );
    assert_eq!(calls(&g, "run_b"), vec![util_g()]);
}

/// A file module's own `use super::f` keeps its meaning: the crate root's
/// `f`, not the namesake in `other.rs`.
#[test]
fn test_build_rust_use_super_in_file_module_binds_parent_file() {
    let g = build(&[
        (
            "src/lib.rs",
            "pub mod a;\npub mod other;\npub fn f() -> i32 { 0 }\n",
        ),
        ("src/a.rs", "use super::f;\npub fn run_a() -> i32 { f() }\n"),
        ("src/other.rs", "pub fn f() -> i32 { 2 }\n"),
    ]);
    assert_eq!(calls(&g, "run_a"), vec![lib("f", 3)]);
}

#[test]
fn test_build_rust_use_super_in_inline_mod_overlay_matches_full_build() {
    let lib_src = "pub mod util;\n\
                   pub mod other;\n\
                   mod inner {\n\
                   \x20   use super::util::g;\n\
                   \x20   pub fn run() -> i32 { g() }\n\
                   }\n";
    let calls = assert_overlay_matches_full(
        &[
            ("src/lib.rs", lib_src),
            ("src/util.rs", UTIL),
            ("src/other.rs", OTHER),
        ],
        "src/lib.rs",
        &["g"],
    );
    assert_eq!(
        calls,
        vec![("run".to_string(), "g".to_string(), 1)],
        "setup: the full build binds run to util's g"
    );
}

/// `a.rs`'s `mod inner { use super::*; }` globs `a`, so `a.rs` imports no
/// other file for it: before the rewrite the glob read `super` from `a.rs`
/// and gave a false `a.rs -> lib.rs` import. A file importing itself gives
/// no edge either.
#[test]
fn test_build_rust_use_super_star_in_inline_mod_imports_no_parent_file() {
    let g = build(&[
        ("src/lib.rs", "pub mod a;\npub fn f() -> i32 { 0 }\n"),
        (
            "src/a.rs",
            "pub fn f() -> i32 { 1 }\nmod inner {\n    use super::*;\n}\n",
        ),
    ]);
    assert_eq!(imports_from(&g, "src/a.rs"), Vec::<Option<String>>::new());
}

/// `use super::{self as p}` in `a::b` aliases `a`: the module path and the
/// name `push_self` splits come from one spelling, `self::a`.
#[test]
fn test_push_nested_use_imports_super_self_alias_in_nested_inline_mod_names_parent_mod() {
    let local = parse(
        "src/lib.rs",
        "mod a {\n    mod b {\n        use super::{self as p};\n    }\n}\n",
    );
    let rows: Vec<(&str, &str, Option<&str>)> = local
        .imports
        .iter()
        .map(|i: &RawImport| {
            (
                i.source.as_str(),
                i.imported_name.as_str(),
                i.alias.as_deref(),
            )
        })
        .collect();
    assert_eq!(rows, vec![("self", "a", Some("p"))]);
}

/// The target file of each `Imports` edge out of the `File` node of `path`.
fn imports_from(g: &ZeroCopyGraph, path: &str) -> Vec<Option<String>> {
    let pool = g.string_pool.as_slice();
    let path_of = |i: u32| {
        let n = &g.nodes[i as usize];
        n.has_owning_file()
            .then(|| g.files[n.file_idx as usize].path.resolve(pool).to_string())
    };
    g.edges
        .iter()
        .filter(|e| e.rel_type == RelType::Imports)
        .filter(|e| g.nodes[e.source as usize].kind == NodeKind::File)
        .filter(|e| path_of(e.source).as_deref() == Some(path))
        .map(|e| path_of(e.target))
        .collect()
}

// ── macro tokens that hide or do not run a call ─────────────────────────────

/// The AST sees no binding inside macro tokens: a `let x` or a closure
/// parameter `x` there hides the parameter `x: A`, so `x.m()` names no
/// `A::m` (line 2). `typed` is the control: no binding hides `x`.
#[test]
fn test_build_rust_receiver_rebound_in_macro_tokens_skips_outer_type() {
    let g = build_lib(
        "pub struct A;\n\
         impl A { pub fn m(&self) -> i32 { 1 } }\n\
         pub struct B;\n\
         impl B { pub fn m(&self) -> i32 { 2 } }\n\
         pub fn by_let(x: A) { println!(\"{}\", { let x = B; x.m() }); }\n\
         pub fn by_closure(x: A) { println!(\"{:?}\", [B].iter().map(|x| x.m())); }\n\
         pub fn typed(x: A) { println!(\"{}\", x.m()); }\n",
    );
    assert_eq!(calls(&g, "typed"), vec![lib("m", 2)], "typed");
    for caller in ["by_let", "by_closure"] {
        assert!(
            !calls(&g, caller).contains(&lib("m", 2)),
            "{caller}: {:?}",
            calls(&g, caller)
        );
    }
}

/// The inner `as Wrong` of the self type does not end the path: the call
/// names the trait after the outermost `as`, `Right` (line 5), in or out of
/// a macro.
#[test]
fn test_build_rust_nested_qualified_self_in_macro_binds_outer_trait() {
    let g = build_lib(
        "pub struct Wrapper<T>(T);\n\
         pub struct Foo;\n\
         pub trait Wrong { type Assoc; }\n\
         impl Wrong for Foo { type Assoc = (); }\n\
         pub trait Right { fn make(); }\n\
         impl Right for Wrapper<()> { fn make() {} }\n\
         pub fn in_macro() { println!(\"{:?}\", <Wrapper<<Foo as Wrong>::Assoc> as Right>::make()); }\n\
         pub fn plain() { <Wrapper<<Foo as Wrong>::Assoc> as Right>::make() }\n",
    );
    assert_eq!(calls(&g, "plain"), vec![lib("make", 5)], "plain");
    assert_eq!(calls(&g, "in_macro"), vec![lib("make", 5)], "in_macro");
}

/// `struct f(u8);` in macro tokens declares a tuple struct, as `fn f()`
/// declares a function: neither calls the crate's `f`.
#[test]
fn test_build_rust_tuple_struct_declaration_in_macro_tokens_is_no_call() {
    let g = build_lib(
        "pub fn f() -> i32 { 0 }\n\
         macro_rules! emit { ($($t:tt)*) => {} }\n\
         pub fn run() { emit!(struct f(u8);); }\n",
    );
    assert!(calls(&g, "run").is_empty(), "{:?}", calls(&g, "run"));
}

/// `stringify!` turns its tokens into a string and runs none of them, at
/// the top or nested in another macro: `f` keeps its reference, never a
/// call.
#[test]
fn test_build_rust_stringify_arguments_are_no_call() {
    let g = build_lib(
        "pub fn f() -> i32 { 0 }\n\
         pub fn top() -> &'static str { stringify!(f()) }\n\
         pub fn nested() -> String { format!(\"{}\", stringify!(f())) }\n",
    );
    for caller in ["top", "nested"] {
        assert!(
            calls(&g, caller).is_empty(),
            "{caller}: {:?}",
            calls(&g, caller)
        );
        assert!(
            referenced(&g, caller).contains(&"f".to_string()),
            "{caller}"
        );
    }
}
