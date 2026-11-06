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

// Stands for: `a/mod.rs` holds module `a`, though its file stem is `mod`
// and no file is named `a`. Expected: the edge to `src/a/mod.rs`.
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
// segment but the member, never `a`'s `top` for `a::missing::top()`.
// Expected: no edge for any spelling.
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
// file confirms, so `self::deep::nope::g()` never reaches the `g` of the
// inline `deep::inner`. Expected: only the spelled path `deep::inner::g`
// binds.
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
// so the confirmation keeps the type route. Expected: all four spellings
// bind it.
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
// before it, so `a::m::S::new()` never reaches the `S` beside `m`.
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
// the type `T` inside `m`.
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
// `impl`, and that `impl` names the item as a type. Expected: the
// associated function.
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

const TWIN_CONFIGS: &str = "pub mod v1 {\n    pub struct Config;\n    \
                            impl Config { pub fn new() -> Self { Config } }\n}\n\
                            pub mod v2 {\n    pub struct Config;\n    \
                            impl Config { pub fn new() -> Self { Config } }\n}\n";

/// `Calls` targets of `caller` as `(file, name, start line)`, sorted and
/// deduplicated: tells apart two same-named members of one file.
fn calls_at(graph: &ZeroCopyGraph, caller: &str) -> Vec<(String, String, u32)> {
    let pool = graph.string_pool.as_slice();
    let mut out: Vec<(String, String, u32)> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .map(|e| {
            let target = &graph.nodes[e.target as usize];
            let file = if target.has_owning_file() {
                graph.files[target.file_idx as usize]
                    .path
                    .resolve(pool)
                    .to_string()
            } else {
                String::new()
            };
            (file, target.name.resolve(pool).to_string(), target.span.0)
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Start lines of every node `name` declared in `file`, sorted.
fn lines_of(graph: &ZeroCopyGraph, file: &str, name: &str) -> Vec<u32> {
    let pool = graph.string_pool.as_slice();
    let mut out: Vec<u32> = graph
        .nodes
        .iter()
        .filter(|n| n.has_owning_file() && n.name.resolve(pool) == name)
        .filter(|n| graph.files[n.file_idx as usize].path.resolve(pool) == file)
        .map(|n| n.span.0)
        .collect();
    out.sort();
    out
}

/// Stands for (A13b, contract change from "no edge"): a path through a type
/// the file declares twice in two inline modules (`api::v1::Config`,
/// `api::v2::Config`) binds the `new` of the module the path names, and never
/// the other twin's `new` (the two `new` nodes share file and name, so the
/// start line tells them apart).
#[test]
fn test_module_path_call_to_type_in_one_of_two_versioned_modules_binds_that_version() {
    let files = [
        (
            "src/lib.rs",
            "pub mod api;\n\
             pub fn use_v1() { api::v1::Config::new(); }\n\
             pub fn use_v2() { api::v2::Config::new(); }\n\
             pub fn use_v2_crate() { crate::api::v2::Config::new(); }\n",
        ),
        ("src/api/mod.rs", TWIN_CONFIGS),
    ];
    let g = crate_graph(&files, true);
    let lines = lines_of(&g, "src/api/mod.rs", "new");
    assert_eq!(lines.len(), 2, "one `new` per version: {lines:?}");
    let at = |line: u32| vec![("src/api/mod.rs".to_string(), "new".to_string(), line)];
    assert_eq!(calls_at(&g, "use_v1"), at(lines[0]), "use_v1");
    assert_eq!(calls_at(&g, "use_v2"), at(lines[1]), "use_v2");
    assert_eq!(calls_at(&g, "use_v2_crate"), at(lines[1]), "use_v2_crate");
}

/// Stands for (A13, contract change from "no edge"): a top-level type and a
/// same-named type inside an inline `mod` of one file are different
/// declarations, so `a::Config::new()` binds the top-level `new` (the later
/// line here) and never `v1::Config`'s `new`.
#[test]
fn test_module_path_call_to_top_level_type_with_nested_twin_binds_the_top_level_member() {
    let files = [
        (
            "src/lib.rs",
            "pub mod a;\npub fn r_top() { a::Config::new(); }\n",
        ),
        (
            "src/a/mod.rs",
            "pub mod v1 { pub struct Config; impl Config { pub fn new() {} } }\n\
             pub struct Config;\nimpl Config { pub fn new() {} }\n",
        ),
    ];
    let g = crate_graph(&files, true);
    let lines = lines_of(&g, "src/a/mod.rs", "new");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(
        calls_at(&g, "r_top"),
        vec![("src/a/mod.rs".to_string(), "new".to_string(), lines[1])]
    );
}

const TWIN_LIB: &str = "mod outer { pub mod a { pub mod b { pub fn wrong() {} } } }\n\
                        mod a { pub mod b { pub fn right() {} } }\n\
                        mod solo { pub fn one() {} }\n\
                        fn caller() { crate::a::b::wrong(); }\n\
                        fn caller2() { crate::a::b::right(); }\n\
                        fn plain() { crate::solo::one(); }\n";

/// Stands for (A13, contract change from "no edge"): an exact `crate::` path
/// through a module name the file declares twice (`outer::a::b` beside
/// `a::b`) binds the member at that exact path. `crate::a::b::wrong()` names
/// no declaration (`wrong` lives at `crate::outer::a::b`), so it binds
/// nothing. A twin-free path in the same file still binds.
#[test]
fn test_module_path_call_through_twin_modules_with_exact_path_binds_right_target() {
    let g = crate_graph(&[("src/lib.rs", TWIN_LIB)], true);
    assert_eq!(targets(&g, "caller2"), vec![to("src/lib.rs", "right")]);
    assert_eq!(targets(&g, "caller"), vec![]);
    assert_eq!(targets(&g, "plain"), vec![to("src/lib.rs", "one")]);
}

/// Stands for: an exact path through twin modules to a member the named
/// module lacks binds nothing (neither twin's namesake is borrowed).
#[test]
fn test_module_path_call_through_twin_modules_to_missing_member_binds_nothing() {
    let lib = "mod outer { pub mod a { pub mod b { pub fn missing() {} } } }\n\
               mod a { pub mod b { pub fn right() {} } }\n\
               fn caller() { crate::a::b::missing(); }\n";
    let g = crate_graph(&[("src/lib.rs", lib)], true);
    assert_eq!(targets(&g, "caller"), vec![]);
}

/// Stands for (control, M08 base): with no twin, the same exact path binds.
#[test]
fn test_module_path_call_without_twin_binds_right_target() {
    let lib = "mod a { pub mod b { pub fn right() {} } }\n\
               fn caller2() { crate::a::b::right(); }\n";
    let g = crate_graph(&[("src/lib.rs", lib)], true);
    assert_eq!(targets(&g, "caller2"), vec![to("src/lib.rs", "right")]);
}

/// Stands for (M08): adding a twin module leaves the verdict of the exact
/// path `crate::a::b::right()` unchanged, and `crate::a::b::wrong()` has no
/// edge in the variant. Variant 2: `#[cfg]` twins of one module path have no
/// resolvable owner, so `crate::sys::f()` keeps "no edge".
#[test]
fn test_metamorphic_twin_module_added_keeps_exact_path_target() {
    let base = "mod a { pub mod b { pub fn right() {} } }\n\
                fn caller2() { crate::a::b::right(); }\n";
    let g_base = crate_graph(&[("src/lib.rs", base)], true);
    let g_twin = crate_graph(&[("src/lib.rs", TWIN_LIB)], true);
    assert_eq!(targets(&g_base, "caller2"), vec![to("src/lib.rs", "right")]);
    assert_eq!(targets(&g_twin, "caller2"), targets(&g_base, "caller2"));
    assert_eq!(targets(&g_twin, "caller"), vec![]);

    let cfg = "#[cfg(unix)]\nmod sys { pub fn f() {} }\n\
               #[cfg(windows)]\nmod sys { pub fn f() {} }\n\
               fn via_sys() { crate::sys::f(); }\n";
    let g_cfg = crate_graph(&[("src/lib.rs", cfg)], true);
    assert_eq!(targets(&g_cfg, "via_sys"), vec![]);
}

// ---- A11: a union inside an inline module

const UNION_LIB: &str =
    "pub mod a;\npub mod b;\npub fn r() { a::m::U::mk(); }\npub fn rs() { a::m::S::mk(); }\n";
const UNION_A: &str = "pub mod m {\n    pub union U { pub x: u32 }\n    \
                       impl U { pub fn mk() -> U { U { x: 0 } } }\n    \
                       pub struct S { pub x: u32 }\n    \
                       impl S { pub fn mk() -> S { S { x: 0 } } }\n}\n";
const UNION_DECOY: &str =
    "pub union U { pub x: u32 }\nimpl U { pub fn mk() -> U { U { x: 0 } } }\n";

fn union_graph() -> ZeroCopyGraph {
    crate_graph(
        &[
            ("src/lib.rs", UNION_LIB),
            ("src/a/mod.rs", UNION_A),
            ("src/b.rs", UNION_DECOY),
        ],
        true,
    )
}

/// Stands for: a union inside an inline module is a confirmable middle
/// segment, so `a::m::U::mk()` binds the union's `mk` (owner `U`).
#[test]
fn test_module_path_call_to_union_in_inline_module_binds_associated_function() {
    let g = union_graph();
    assert_eq!(targets(&g, "r"), vec![to("src/a/mod.rs", "mk")]);
    assert_eq!(call_owners(&g, "r"), vec!["U".to_string()]);
}

/// Stands for: the decoy union of the same name in `b.rs` is never bound.
#[test]
fn test_module_path_call_to_union_in_inline_module_ignores_namesake_union_in_other_file() {
    let g = union_graph();
    assert!(
        !targets(&g, "r").contains(&to("src/b.rs", "mk")),
        "{:?}",
        targets(&g, "r")
    );
    assert!(
        !targets(&g, "rs").contains(&to("src/b.rs", "mk")),
        "{:?}",
        targets(&g, "rs")
    );
    // POSITIVE CONTROL: the union's own edge exists.
    assert_eq!(targets(&g, "r"), vec![to("src/a/mod.rs", "mk")]);
}

/// Stands for (control): a struct in the same inline module binds its `mk`.
#[test]
fn test_module_path_call_to_struct_in_inline_module_binds_associated_function() {
    let g = union_graph();
    assert_eq!(targets(&g, "rs"), vec![to("src/a/mod.rs", "mk")]);
    assert_eq!(call_owners(&g, "rs"), vec!["S".to_string()]);
}

/// Stands for: the same union in an inline module of `lib.rs` itself binds.
#[test]
fn test_module_path_call_to_union_in_inline_module_of_lib_binds_associated_function() {
    let lib = "pub mod m {\n    pub union U { pub x: u32 }\n    \
               impl U { pub fn mk() -> U { U { x: 0 } } }\n}\n\
               pub fn r2() { m::U::mk(); }\n";
    let g = crate_graph(&[("src/lib.rs", lib)], true);
    assert_eq!(targets(&g, "r2"), vec![to("src/lib.rs", "mk")]);
}

/// Stands for: a union has a real type declaration node (a qualifier kind
/// named `U`), so the `Impl` node no longer stands in for it.
#[test]
fn test_parse_file_rust_union_emits_declaration_node() {
    let local = RustProvider::new()
        .expect("RustProvider init")
        .parse_file(Path::new("src/lib.rs"), b"pub union U { pub x: u32 }\n")
        .expect("parse");
    let declared: Vec<_> = local
        .nodes
        .iter()
        .filter(|n| n.name == "U" && n.kind.is_qualifier())
        .collect();
    assert_eq!(declared.len(), 1, "nodes: {:?}", local.nodes);
}

// ---- A15c: a glob import

const GLOB_OTHER_MOD: &str = "pub mod registry;\n";
const GLOB_REGISTRY: &str = "pub fn lookup() {}\n";

fn glob_files<'a>(lib: &'a str) -> [(&'a str, &'a str); 4] {
    [
        ("src/lib.rs", lib),
        ("src/other/mod.rs", GLOB_OTHER_MOD),
        ("src/other/registry.rs", GLOB_REGISTRY),
        // not declared by any `mod`: not part of the crate, stem-matches `registry`
        ("src/registry.rs", GLOB_REGISTRY),
    ]
}

/// Stands for: `use other::*;` names no head, and the undeclared file
/// `src/registry.rs` is no proof for `registry::lookup()`; the decoy never binds.
#[test]
fn test_module_path_call_through_glob_import_does_not_bind_undeclared_namesake_file() {
    let lib = "mod other;\nuse other::*;\npub fn run() { registry::lookup(); }\n";
    let g = crate_graph(&glob_files(lib), true);
    assert!(
        !targets(&g, "run").contains(&to("src/registry.rs", "lookup")),
        "{:?}",
        targets(&g, "run")
    );
}

/// Stands for (section 00 decision): a glob import resolves its members, so
/// `registry::lookup()` binds `src/other/registry.rs`.
#[test]
fn test_module_path_call_through_glob_import_binds_glob_member() {
    let lib = "mod other;\nuse other::*;\npub fn run() { registry::lookup(); }\n";
    let g = crate_graph(&glob_files(lib), true);
    assert_eq!(
        targets(&g, "run"),
        vec![to("src/other/registry.rs", "lookup")]
    );
}

/// Stands for (control): a named import stops the decoy and binds the
/// imported module.
#[test]
fn test_module_path_call_through_named_import_binds_imported_module() {
    let lib = "mod other;\nuse other::registry;\npub fn run() { registry::lookup(); }\n";
    let g = crate_graph(&glob_files(lib), true);
    assert_eq!(
        targets(&g, "run"),
        vec![to("src/other/registry.rs", "lookup")]
    );
}

// ---- A15d: a bare path with no Cargo.toml

fn bare_files() -> [(&'static str, &'static str); 4] {
    [
        ("src/lib.rs", MOD_RS_LIB),
        ("src/a/mod.rs", MOD_RS_A),
        ("src/a/b.rs", MOD_RS_B),
        ("src/c/mod.rs", MOD_RS_C),
    ]
}

/// Stands for: with no manifest, `pub mod a;` in `lib.rs` still proves
/// module `a`, so `a::top()` binds `src/a/mod.rs`.
#[test]
fn test_bare_module_path_call_without_manifest_binds_declared_module() {
    let g = crate_graph(&bare_files(), false);
    assert_eq!(targets(&g, "run"), vec![to("src/a/mod.rs", "top")]);
}

/// Stands for: the same-named `top` of module `c` is never borrowed.
#[test]
fn test_bare_module_path_call_without_manifest_ignores_same_named_module_elsewhere() {
    let g = crate_graph(&bare_files(), false);
    assert!(
        !targets(&g, "run").contains(&to("src/c/mod.rs", "top")),
        "{:?}",
        targets(&g, "run")
    );
    // POSITIVE CONTROL: the declared module's edge exists.
    assert!(!targets(&g, "run").is_empty());
}

/// Stands for: with no `mod a;` declaration anywhere, a stray `src/a/mod.rs`
/// is no proof; no edge at all.
#[test]
fn test_bare_module_path_call_without_manifest_or_mod_declaration_binds_nothing() {
    let files = [
        ("src/lib.rs", "pub fn run() { a::top(); }\n"),
        ("src/a/mod.rs", MOD_RS_A),
    ];
    let g = crate_graph(&files, false);
    assert_eq!(targets(&g, "run"), vec![]);
}

/// Stands for (control): the manifest build of the same fixture binds `a/mod.rs`.
#[test]
fn test_bare_module_path_call_with_manifest_binds_declared_module() {
    let g = crate_graph(&bare_files(), true);
    assert_eq!(targets(&g, "run"), vec![to("src/a/mod.rs", "top")]);
}
