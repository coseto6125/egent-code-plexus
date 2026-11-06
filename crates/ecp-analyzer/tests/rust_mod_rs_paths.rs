//! A Rust path call `a::top()` binds the function in module `a` whatever
//! file layout holds the module: `a.rs`, `a/mod.rs` (also when it declares a
//! child module), or an inline `mod a { .. }`.
//!
//! Contract the assertions stand for: the `Calls` edge of the caller reaches
//! the module's own `top`, never a same-named `top` of another module, and an
//! unplaceable module records no edge. Only Rust has `mod.rs` and `crate::` /
//! `self::` / `super::` anchors; the other 13 mainstream languages have no
//! such shape, so this file adds no case for them.

use crate::name_ref_support::{calls, refs};
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use std::path::Path;

const MANIFEST: &str = "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

/// The files written to a temp crate (so the module tree sees a manifest
/// when `manifest` is set) and indexed through the real pipeline.
fn crate_graph(files: &[(&str, &str)], manifest: bool) -> ZeroCopyGraph {
    let tmp = tempfile::tempdir().expect("tempdir");
    let provider = RustProvider::new().expect("RustProvider init");
    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    let manifest_file = [("Cargo.toml", MANIFEST)];
    let manifest_files: &[(&str, &str)] = if manifest { &manifest_file } else { &[] };
    for &(path, source) in manifest_files.iter().chain(files) {
        let disk_path = tmp.path().join(path);
        std::fs::create_dir_all(disk_path.parent().expect("parent")).expect("create parent");
        std::fs::write(&disk_path, source).expect("write fixture");
        if path.ends_with(".rs") {
            builder.add_graph(
                provider
                    .parse_file(Path::new(path), source.as_bytes())
                    .expect("parse Rust fixture"),
            );
        }
    }
    builder.build()
}

fn to(file: &str, name: &str) -> (String, String) {
    (file.to_string(), name.to_string())
}

/// `calls(caller)` with repeats of one call site collapsed.
fn targets(graph: &ZeroCopyGraph, caller: &str) -> Vec<(String, String)> {
    let mut out = calls(graph, caller);
    out.dedup();
    out
}

const MOD_RS_LIB: &str = "pub mod a;\npub mod c;\npub fn run() { a::top(); }\n";
const MOD_RS_A: &str = "pub mod b;\npub fn top() {}\n";
const MOD_RS_B: &str = "pub fn deep() {}\n";
const MOD_RS_C: &str = "pub fn top() {}\n";

fn mod_rs_layout(lib: &str) -> [(&str, &str); 4] {
    [
        ("src/lib.rs", lib),
        ("src/a/mod.rs", MOD_RS_A),
        ("src/a/b/mod.rs", MOD_RS_B),
        ("src/c/mod.rs", MOD_RS_C),
    ]
}

// main: no `run -> top` edge, because `a/mod.rs` is filed under the stem
// `mod` and no file is named `a`. Expected: the edge to `src/a/mod.rs`.
#[test]
fn test_bare_module_path_call_into_mod_rs_with_child_module_binds_mod_rs() {
    let g = crate_graph(&mod_rs_layout(MOD_RS_LIB), true);
    assert_eq!(targets(&g, "run"), vec![to("src/a/mod.rs", "top")]);
}

#[test]
fn test_anchored_module_path_calls_into_mod_rs_bind_mod_rs() {
    let lib = "pub mod a;\npub mod c;\n\
               pub fn run() { crate::a::top(); self::a::top(); a::b::deep(); crate::a::b::deep(); }\n";
    let g = crate_graph(&mod_rs_layout(lib), true);
    assert_eq!(
        targets(&g, "run"),
        vec![to("src/a/b/mod.rs", "deep"), to("src/a/mod.rs", "top")]
    );
}

// The bare path takes the module-tree tier like `crate::a::top()`, so both
// edges carry one confidence.
#[test]
fn test_bare_module_path_call_into_mod_rs_has_the_confidence_of_the_crate_path() {
    let confidence = |call: &str| {
        let lib = format!("pub mod a;\npub mod c;\npub fn run() {{ {call}; }}\n");
        let g = crate_graph(&mod_rs_layout(&lib), true);
        let edges = refs(&g, "run", RelType::Calls, "call");
        assert_eq!(edges.len(), 1, "{call}: {edges:?}");
        edges[0].3
    };
    assert_eq!(confidence("a::top()"), confidence("crate::a::top()"));
    assert_eq!(confidence("a::top()"), confidence("self::a::top()"));
}

#[test]
fn test_super_call_from_nested_mod_rs_binds_parent_mod_rs() {
    let mut files = mod_rs_layout(MOD_RS_LIB).to_vec();
    files[2] = ("src/a/b/mod.rs", "pub fn go() { super::top(); }\n");
    let g = crate_graph(&files, true);
    assert_eq!(targets(&g, "go"), vec![to("src/a/mod.rs", "top")]);
}

#[test]
fn test_module_path_call_with_same_named_function_elsewhere_binds_only_own_module() {
    let lib = "pub mod a;\npub mod c;\npub fn run() { c::top(); }\n";
    let g = crate_graph(&mod_rs_layout(lib), true);
    assert_eq!(targets(&g, "run"), vec![to("src/c/mod.rs", "top")]);
}

#[test]
fn test_module_path_call_to_member_the_module_lacks_records_no_edge() {
    let lib = "pub mod a;\npub mod c;\npub fn run() { a::missing(); }\n";
    let g = crate_graph(&mod_rs_layout(lib), true);
    assert_eq!(targets(&g, "run"), vec![]);
}

#[test]
fn test_module_path_call_into_empty_mod_rs_does_not_borrow_another_modules_function() {
    let files = [
        (
            "src/lib.rs",
            "pub mod a;\npub mod c;\npub fn run() { a::top(); }\n",
        ),
        ("src/a/mod.rs", ""),
        ("src/c/mod.rs", MOD_RS_C),
    ];
    let g = crate_graph(&files, true);
    assert_eq!(targets(&g, "run"), vec![]);
}

#[test]
fn test_module_path_call_with_absent_module_records_no_edge() {
    let lib = "pub mod c;\npub fn run() { zz::top(); }\n";
    let files = [("src/lib.rs", lib), ("src/c/mod.rs", MOD_RS_C)];
    let g = crate_graph(&files, true);
    assert_eq!(targets(&g, "run"), vec![]);
}

#[test]
fn test_module_path_call_with_two_mod_rs_of_one_name_records_no_edge() {
    let files = [
        (
            "src/lib.rs",
            "pub mod p;\npub mod q;\npub fn run() { x::f(); }\n",
        ),
        ("src/p/mod.rs", "pub mod x;\n"),
        ("src/p/x/mod.rs", "pub fn f() {}\n"),
        ("src/q/mod.rs", "pub mod x;\n"),
        ("src/q/x/mod.rs", "pub fn f() {}\n"),
    ];
    let g = crate_graph(&files, true);
    assert_eq!(targets(&g, "run"), vec![]);
}

#[test]
fn test_module_path_call_repeated_twice_binds_the_same_target() {
    let lib = "pub mod a;\npub mod c;\npub fn run() { a::top(); a::top(); }\n";
    let g = crate_graph(&mod_rs_layout(lib), true);
    assert_eq!(targets(&g, "run"), vec![to("src/a/mod.rs", "top")]);
}

#[test]
fn test_module_path_calls_with_file_per_module_layout_bind_the_module_files() {
    let lib = "pub mod a;\npub mod c;\n\
               pub fn run() { a::top(); crate::a::top(); self::a::top(); }\n";
    let files = [
        ("src/lib.rs", lib),
        ("src/a.rs", MOD_RS_A),
        ("src/a/b.rs", "pub fn go() { super::top(); }\n"),
        ("src/c.rs", MOD_RS_C),
    ];
    let g = crate_graph(&files, true);
    assert_eq!(targets(&g, "run"), vec![to("src/a.rs", "top")]);
    assert_eq!(targets(&g, "go"), vec![to("src/a.rs", "top")]);
}

#[test]
fn test_module_path_calls_into_inline_module_bind_the_inline_function() {
    let lib = "mod a { pub fn top() {} }\nmod c { pub fn top() {} }\n\
               pub fn run() { a::top(); crate::a::top(); self::a::top(); }\n";
    let g = crate_graph(&[("src/lib.rs", lib)], true);
    let hits = calls(&g, "run");
    assert!(!hits.is_empty(), "inline module call must bind");
    assert!(hits.iter().all(|hit| hit == &to("src/lib.rs", "top")));
}

// Stands for: a path binds only when the tree or the file confirms every
// segment but the member. main binds `crate::a::missing::top()` to `a`'s
// `top`, and this branch's `self::` retry also binds the bare
// `a::missing::top()`. Expected: no edge for either spelling.
#[test]
fn test_module_path_call_with_missing_middle_segment_records_no_edge() {
    for call in [
        "a::missing::top()",
        "crate::a::missing::top()",
        "self::a::missing::top()",
    ] {
        let lib = format!("pub mod a;\npub mod c;\npub fn run() {{ {call}; }}\n");
        let g = crate_graph(&mod_rs_layout(&lib), true);
        assert_eq!(targets(&g, "run"), vec![], "{call}");
    }
}

// Stands for: an inline `mod` past the deepest file module is a segment the
// file confirms. main binds `self::deep::nope::g()` to the `g` of the inline
// `deep::inner`. Expected: only the spelled path `deep::inner::g` binds.
#[test]
fn test_module_path_call_through_inline_modules_binds_only_declared_segments() {
    let lib = "mod deep { pub mod inner { pub fn g() {} } }\n\
               pub fn run() { self::deep::inner::g(); crate::deep::inner::g(); }\n\
               pub fn bad() { self::deep::nope::g(); crate::deep::nope::g(); }\n";
    let g = crate_graph(&[("src/lib.rs", lib)], true);
    assert_eq!(targets(&g, "run"), vec![to("src/lib.rs", "g")]);
    assert_eq!(targets(&g, "bad"), vec![]);
}

// Stands for: a type segment past the module still reaches its associated
// function (`a::S::new()`), also through a turbofish (`a::S::<u8>::new()`),
// so the confirmation keeps the type route. main binds the plain spellings;
// this branch binds all four.
#[test]
fn test_module_path_call_to_associated_function_of_module_type_binds_it() {
    let files = [
        (
            "src/lib.rs",
            "pub mod a;\npub fn run() { a::S::new(); crate::a::S::new(); \
             a::S::<u8>::new(); crate::a::S::<u8>::new(); }\n",
        ),
        (
            "src/a/mod.rs",
            "pub struct S<T>(T);\nimpl<T> S<T> { pub fn new() {} }\n",
        ),
    ];
    let g = crate_graph(&files, true);
    assert_eq!(targets(&g, "run"), vec![to("src/a/mod.rs", "new")]);
}

/// The owner of each `Calls` target of `caller`, sorted; `""` for none.
fn call_owners(graph: &ZeroCopyGraph, caller: &str) -> Vec<String> {
    let pool = graph.string_pool.as_slice();
    let mut out: Vec<String> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .map(|e| {
            graph.nodes[e.target as usize]
                .owner_class
                .resolve(pool)
                .to_string()
        })
        .collect();
    out.sort();
    out
}

const NESTED_A: &str = "pub struct S;\npub struct Other;\nimpl S { pub fn new() {} }\n\
                        pub fn f() {}\n\
                        pub mod m { pub fn f() {} pub struct T; impl T { pub fn make() {} } }\n";

// Stands for: each segment past the item must be declared inside the one
// before it. HEAD binds `a::m::S::new()` and `crate::a::m::S::new()` to
// `S::new` (the `S` beside `m`, not in it); main binds the first.
// Expected: no edge for any spelling, `a::S::Other::new()` included.
#[test]
fn test_module_path_call_through_segment_not_declared_inside_previous_records_no_edge() {
    let files = [
        (
            "src/lib.rs",
            "pub mod a;\npub fn bad() { a::m::S::new(); crate::a::m::S::new(); \
             a::S::Other::new(); }\n",
        ),
        ("src/a/mod.rs", NESTED_A),
    ];
    let g = crate_graph(&files, true);
    assert_eq!(targets(&g, "bad"), vec![]);
}

// Stands for: the member comes from the last qualifier. `a::m::f()` binds
// `m`'s own `f`, never the `f` beside `m`, and `a::m::T::make()` reaches
// the type `T` inside `m`. HEAD and main bind `m::f` (another tier, 0.85);
// HEAD binds `T::make`, main none.
#[test]
fn test_module_path_call_into_inline_module_member_binds_the_last_qualifiers_member() {
    let files = [
        (
            "src/lib.rs",
            "pub mod a;\npub fn run() { a::m::f(); }\npub fn mk() { a::m::T::make(); }\n",
        ),
        ("src/a/mod.rs", NESTED_A),
    ];
    let g = crate_graph(&files, true);
    assert_eq!(targets(&g, "run"), vec![to("src/a/mod.rs", "f")]);
    assert_eq!(call_owners(&g, "run"), vec!["m".to_string()]);
    assert_eq!(targets(&g, "mk"), vec![to("src/a/mod.rs", "make")]);
    assert_eq!(call_owners(&g, "mk"), vec!["T".to_string()]);
}

// Stands for: the Rust parser files no node for a `union`, only its
// `impl`, and that `impl` names the item as a type. HEAD and main bind
// nothing; expected: the associated function.
#[test]
fn test_module_path_call_to_union_associated_function_binds_it() {
    let files = [
        (
            "src/lib.rs",
            "pub mod a;\npub fn r_union() { a::U::mk(); crate::a::U::mk(); }\n",
        ),
        (
            "src/a/mod.rs",
            "pub union U { x: u32 }\nimpl U { pub fn mk() -> U { U { x: 0 } } }\n",
        ),
    ];
    let g = crate_graph(&files, true);
    assert_eq!(targets(&g, "r_union"), vec![to("src/a/mod.rs", "mk")]);
}
