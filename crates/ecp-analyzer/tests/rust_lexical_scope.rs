//! Rust function-local functions and closures are lexically scoped (design A,
//! slice R6): `lexical_scope_kinds(Rust)` is `[Function]`, so a `Function`
//! whose innermost container is a callable has no owner, a lexical-parent
//! row, and a uid hashed over its parent's anchor. Only the lexical tier
//! binds it (I5); a module member keeps its module owner and stays global.
//!
//! Every assertion reads what a consumer reads: the stored owner and uid,
//! the archived lexical-parent table and display owner, and the edges.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::identity::scoped_uid;
use ecp_core::graph::{ArchivedZeroCopyGraph, Edge, NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

const PATH: &str = "test.rs";
const SRC: &str = r#"fn helper() -> i32 { 1 }

fn outer() -> i32 {
    fn helper() -> i32 { 2 }
    helper()
}
"#;

fn parse_at(path: &str, src: &str) -> LocalGraph {
    RustProvider::new()
        .expect("RustProvider::new")
        .parse_file(Path::new(path), src.as_bytes())
        .expect("parse_file")
}

fn parse() -> LocalGraph {
    parse_at(PATH, SRC)
}

fn build(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for &(path, src) in files {
        builder.add_graph(parse_at(path, src));
    }
    builder.build()
}

fn graph() -> ZeroCopyGraph {
    build(&[(PATH, SRC)])
}

fn archive(g: &ZeroCopyGraph) -> rkyv::util::AlignedVec {
    rkyv::to_bytes::<rkyv::rancor::Error>(g).expect("archive graph")
}

fn access(bytes: &[u8]) -> &ArchivedZeroCopyGraph {
    rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).expect("access graph")
}

/// One non-File node as the builder stores it; `line` is 1-based.
#[derive(Debug)]
struct Row {
    name: String,
    kind: NodeKind,
    line: u32,
    uid: u64,
}

fn rows(g: &ZeroCopyGraph) -> Vec<Row> {
    let pool = g.string_pool.as_slice();
    let mut out: Vec<Row> = g
        .nodes
        .iter()
        .filter(|n| n.kind != NodeKind::File)
        .map(|n| Row {
            name: n.name.resolve(pool).to_string(),
            kind: n.kind,
            line: n.span.0 + 1,
            uid: n.uid,
        })
        .collect();
    out.sort_by_key(|r| r.line);
    out
}

fn at(rows: &[Row], line: u32) -> &Row {
    rows.iter()
        .find(|r| r.line == line)
        .unwrap_or_else(|| panic!("no node at line {line}: {rows:?}"))
}

fn uid_of(kind: NodeKind, owner: Option<&str>, name: &str) -> u64 {
    ecp_core::uid::compute(kind, PATH, owner, name)
}

/// The index of the node named `name` that starts on 1-based `line` of `path`.
fn node(g: &ZeroCopyGraph, path: &str, name: &str, line: u32) -> u32 {
    let pool = g.string_pool.as_slice();
    g.nodes
        .iter()
        .position(|n| {
            n.kind != NodeKind::File
                && n.name.resolve(pool) == name
                && n.span.0 + 1 == line
                && g.files[n.file_idx as usize].path.resolve(pool) == path
        })
        .unwrap_or_else(|| panic!("setup: {path}:{line} declares `{name}`")) as u32
}

/// Every node named `name` in source order.
fn all_named(g: &ZeroCopyGraph, name: &str) -> Vec<u32> {
    let pool = g.string_pool.as_slice();
    let mut out: Vec<u32> = (0..g.nodes.len() as u32)
        .filter(|&i| g.nodes[i as usize].name.resolve(pool) == name)
        .collect();
    out.sort_by_key(|&i| g.nodes[i as usize].span);
    out
}

fn owner(g: &ZeroCopyGraph, idx: u32) -> &str {
    g.nodes[idx as usize]
        .owner_class
        .resolve(g.string_pool.as_slice())
}

fn edges_from(g: &ZeroCopyGraph, rel: RelType, source: u32) -> Vec<&Edge> {
    g.edges
        .iter()
        .filter(|e| e.rel_type == rel && e.source == source)
        .collect()
}

/// The targets of `source`'s `Calls` edges whose target is named `name`.
fn calls_named(g: &ZeroCopyGraph, source: u32, name: &str) -> Vec<(u32, f32)> {
    let pool = g.string_pool.as_slice();
    edges_from(g, RelType::Calls, source)
        .into_iter()
        .filter(|e| g.nodes[e.target as usize].name.resolve(pool) == name)
        .map(|e| (e.target, e.confidence))
        .collect()
}

fn has_edge(g: &ZeroCopyGraph, rel: RelType, source: u32, target: u32) -> bool {
    g.edges
        .iter()
        .any(|e| e.rel_type == rel && e.source == source && e.target == target)
}

fn has_edge_into(g: &ZeroCopyGraph, rel: RelType, target: u32) -> bool {
    g.edges
        .iter()
        .any(|e| e.rel_type == rel && e.target == target)
}

/// Whether `outer`'s span strictly holds `inner`'s.
fn contains(g: &ZeroCopyGraph, outer: u32, inner: u32) -> bool {
    let (o, i) = (g.nodes[outer as usize].span, g.nodes[inner as usize].span);
    (o.0, o.1) <= (i.0, i.1) && (i.2, i.3) <= (o.2, o.3) && o != i
}

/// Stands for: the nested `helper` is a scoped node, its uid hashed over
/// `outer`'s anchor, so a rename or move of an unrelated sibling never moves
/// it. HEAD stored the owner `outer` in the uid instead.
#[test]
fn test_build_rust_nested_fn_same_name_as_top_level_scoped_uid() {
    let rows = rows(&graph());
    let top = at(&rows, 1);
    let nested = at(&rows, 4);
    assert_eq!(
        (top.name.as_str(), top.kind),
        ("helper", NodeKind::Function)
    );
    assert_eq!(top.uid, uid_of(NodeKind::Function, None, "helper"));
    assert_eq!(
        (nested.name.as_str(), nested.kind),
        ("helper", NodeKind::Function)
    );
    let outer_anchor = uid_of(NodeKind::Function, None, "outer");
    assert_eq!(
        nested.uid,
        scoped_uid(outer_anchor, NodeKind::Function, "helper")
    );
    assert_ne!(
        nested.uid,
        uid_of(NodeKind::Function, Some("outer"), "helper")
    );
    assert_ne!(top.uid, nested.uid);
    assert!(
        rows.iter().all(|r| !r.name.is_empty()),
        "no tombstone: {rows:?}"
    );
}

/// Stands for: the parser stores no owner on the nested fn (I1), and the
/// graph places it by its lexical-parent row; readers print `outer` from the
/// display owner. HEAD stored the owner `outer` and wrote no row.
#[test]
fn test_parse_file_rust_nested_fn_owner_none_lexical_parent_outer() {
    let local = parse();
    let helpers: Vec<_> = {
        let mut h: Vec<_> = local.nodes.iter().filter(|n| n.name == "helper").collect();
        h.sort_by_key(|n| n.span.0);
        h
    };
    assert_eq!(helpers.len(), 2);
    assert_eq!(helpers[0].owner_class, None);
    assert_eq!(helpers[1].owner_class, None);

    let g = graph();
    let (top, outer, nested) = (
        node(&g, PATH, "helper", 1),
        node(&g, PATH, "outer", 3),
        node(&g, PATH, "helper", 4),
    );
    let bytes = archive(&g);
    let archived = access(&bytes);
    assert_eq!(archived.lexical_parent(nested), Some(outer));
    assert_eq!(archived.display_owner(nested).as_deref(), Some("outer"));
    assert!(!archived.is_lexically_scoped(top));
    assert!(!archived.is_lexically_scoped(outer));
    assert_eq!(archived.lexical_parents.len(), 1, "one scoped node");
}

/// Stands for: `outer`'s bare `helper()` binds the nested `helper` through
/// the lexical tier at confidence 1.0, never the top-level one. The scoped
/// node gets no `File -Defines->` and no `outer -HasMethod->` edge. HEAD
/// bound it the same way but its target carried the owner `outer`.
#[test]
fn test_build_rust_call_to_bare_helper_binds_nested_through_lexical_tier() {
    let g = graph();
    let (top, outer, nested) = (
        node(&g, PATH, "helper", 1),
        node(&g, PATH, "outer", 3),
        node(&g, PATH, "helper", 4),
    );
    assert_eq!(calls_named(&g, outer, "helper"), vec![(nested, 1.0)]);
    assert_eq!(owner(&g, nested), "");
    assert!(!has_edge_into(&g, RelType::Calls, top));
    assert!(!has_edge_into(&g, RelType::HasMethod, nested));
    assert!(!has_edge_into(&g, RelType::Defines, nested));
    assert!(has_edge_into(&g, RelType::Defines, top));
}

/// Stands for: a nested fn is invisible to the name tiers (I5). A bare
/// `helper()` in another file never binds the `helper` nested in `a`; with
/// a top-level `helper` elsewhere it binds that one.
#[test]
fn test_build_rust_nested_fn_other_file_bare_call_skips_scoped() {
    let a = ("a.rs", "fn a() {\n    fn helper() {}\n    helper();\n}\n");
    let b = ("b.rs", "fn b() {\n    helper();\n}\n");
    let c = ("c.rs", "pub fn helper() {}\n");

    let g = build(&[a, b]);
    let (nested, caller) = (node(&g, "a.rs", "helper", 2), node(&g, "b.rs", "b", 1));
    assert_eq!(calls_named(&g, caller, "helper"), vec![]);
    assert_eq!(
        calls_named(&g, node(&g, "a.rs", "a", 1), "helper"),
        vec![(nested, 1.0)]
    );

    let g = build(&[a, b, c]);
    let (top, caller) = (node(&g, "c.rs", "helper", 1), node(&g, "b.rs", "b", 1));
    let targets: Vec<u32> = calls_named(&g, caller, "helper")
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    assert_eq!(targets, vec![top]);
}

/// `struct A;` and one `impl std::fmt::<Trait> for A` per trait, each `fmt`
/// declaring and calling its own `pad`.
fn trait_impls(traits: &[&str]) -> String {
    let mut src = String::from("struct A;\n");
    for (i, t) in traits.iter().enumerate() {
        src += &format!(
            "impl std::fmt::{t} for A {{\n    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {{\n        fn pad() -> usize {{ {i} }}\n        pad();\n        Ok(())\n    }}\n}}\n"
        );
    }
    src
}

/// Stands for (R-F1): two or three trait impls of one type each nest a
/// `pad` in their `fmt`. Each `pad` is a distinct node whose lexical parent
/// is its own `fmt`, each `fmt` calls its own `pad` alone, and no `pad` is a
/// member of anything. HEAD placed them by the owner text
/// `std::fmt::Display::A::fmt` and wrote no lexical-parent row.
#[test]
fn test_build_rust_trait_impls_nested_pad_bound_to_own_fmt() {
    for traits in [
        &["Display", "Debug"][..],
        &["Display", "Debug", "LowerHex"][..],
    ] {
        let src = trait_impls(traits);
        let g = build(&[(PATH, &src)]);
        let bytes = archive(&g);
        let archived = access(&bytes);
        let (fmts, pads) = (all_named(&g, "fmt"), all_named(&g, "pad"));
        assert_eq!(fmts.len(), traits.len(), "{traits:?}: one `fmt` per impl");
        assert_eq!(pads.len(), traits.len(), "{traits:?}: one `pad` per `fmt`");
        let mut uids: Vec<u64> = pads.iter().map(|&p| g.nodes[p as usize].uid).collect();
        uids.sort_unstable();
        uids.dedup();
        assert_eq!(uids.len(), traits.len(), "{traits:?}: distinct `pad` uids");
        for (&fmt, &pad) in fmts.iter().zip(&pads) {
            assert!(contains(&g, fmt, pad), "setup: source order pairs them");
            assert_eq!(
                owner(&g, pad),
                "",
                "{traits:?}: a scoped `pad` has no owner"
            );
            assert_eq!(archived.lexical_parent(pad), Some(fmt));
            assert_eq!(
                archived.display_owner(pad).as_deref(),
                Some(format!("{}::fmt", owner(&g, fmt)).as_str())
            );
            assert_eq!(calls_named(&g, fmt, "pad"), vec![(pad, 1.0)], "{traits:?}");
            assert!(!has_edge_into(&g, RelType::HasMethod, pad));
            assert!(!has_edge_into(&g, RelType::Defines, pad));
        }
    }
}

/// Stands for (R-F2): a fn nested in a trait default method is scoped under
/// that method; the trait gains no `HasMethod` edge to it. HEAD's span
/// containment pass gave `T -HasMethod-> helper`.
#[test]
fn test_build_rust_trait_default_method_nested_fn_not_trait_member() {
    let src = "trait T {\n    fn d(&self) -> i32 {\n        fn helper() -> i32 { 1 }\n        helper()\n    }\n}\n";
    let g = build(&[(PATH, src)]);
    let (d, helper) = (node(&g, PATH, "d", 2), node(&g, PATH, "helper", 3));
    let bytes = archive(&g);
    let archived = access(&bytes);
    assert_eq!(owner(&g, helper), "");
    assert_eq!(archived.lexical_parent(helper), Some(d));
    assert_eq!(archived.display_owner(helper).as_deref(), Some("T::d"));
    assert_eq!(calls_named(&g, d, "helper"), vec![(helper, 1.0)]);
    assert!(!has_edge_into(&g, RelType::HasMethod, helper));
    assert!(!archived.is_lexically_scoped(d));
}

/// Stands for (R-F2): a method of an `impl` written inside a fn is no
/// scoped node (its innermost container is the `impl`), and keeps its type
/// owner; a fn nested in that method is scoped under it.
#[test]
fn test_build_rust_impl_in_fn_method_unscoped_nested_fn_scoped() {
    let src = "struct S;\nfn outer() {\n    impl S {\n        fn m(&self) -> i32 {\n            fn h() -> i32 { 1 }\n            h()\n        }\n    }\n}\n";
    let g = build(&[(PATH, src)]);
    let (m, h) = (node(&g, PATH, "m", 4), node(&g, PATH, "h", 5));
    let bytes = archive(&g);
    let archived = access(&bytes);
    assert_eq!(owner(&g, m), "S");
    assert!(!archived.is_lexically_scoped(m));
    assert_eq!(archived.lexical_parent(h), Some(m));
    assert_eq!(archived.display_owner(h).as_deref(), Some("S::m"));
    assert_eq!(calls_named(&g, m, "h"), vec![(h, 1.0)]);
    assert!(!has_edge_into(&g, RelType::HasMethod, h));
}

/// Stands for (R-F2): the members of a module declared inside a fn are no
/// scoped nodes; they keep the module owner `m`, and the module defines
/// them. HEAD stored the owner `outer::m`, which matched no module, so no
/// `Module m -Defines-> f` edge existed.
#[test]
fn test_build_rust_module_in_fn_members_owned_by_module() {
    let src = "fn outer() {\n    mod m {\n        pub struct S;\n        impl S {\n            pub fn new() -> S { S }\n        }\n        pub fn f() {}\n    }\n}\n";
    let g = build(&[(PATH, src)]);
    let (module, new, f) = (
        node(&g, PATH, "m", 2),
        node(&g, PATH, "new", 5),
        node(&g, PATH, "f", 7),
    );
    let bytes = archive(&g);
    let archived = access(&bytes);
    assert_eq!(owner(&g, f), "m");
    assert_eq!(owner(&g, new), "S");
    assert!(!archived.is_lexically_scoped(f));
    assert!(!archived.is_lexically_scoped(new));
    assert_eq!(
        g.nodes[f as usize].uid,
        uid_of(NodeKind::Function, Some("m"), "f")
    );
    assert!(has_edge(&g, RelType::Defines, module, f));
}

/// Stands for (R-F3): a closure in a fn body is scoped under the fn (no
/// owner), and its bare call binds the fn's nested `helper` through the
/// lexical tier, as the fn's own call does.
#[test]
fn test_build_rust_closure_in_fn_scoped_calls_nested_helper() {
    let src = "fn helper() -> i32 { 1 }\nfn outer() -> i32 {\n    fn helper() -> i32 { 2 }\n    let xs = vec![1];\n    let _: Vec<i32> = xs.iter().map(|x| helper() + x).collect();\n    helper()\n}\n";
    let g = build(&[(PATH, src)]);
    let (outer, nested) = (node(&g, PATH, "outer", 2), node(&g, PATH, "helper", 3));
    let pool = g.string_pool.as_slice();
    let closure = (0..g.nodes.len() as u32)
        .find(|&i| {
            g.nodes[i as usize]
                .name
                .resolve(pool)
                .starts_with("<anonymous:5:")
        })
        .unwrap_or_else(|| panic!("setup: the closure on line 5 is a node"));
    let bytes = archive(&g);
    let archived = access(&bytes);
    assert_eq!(owner(&g, closure), "");
    assert_eq!(archived.lexical_parent(closure), Some(outer));
    assert_eq!(calls_named(&g, closure, "helper"), vec![(nested, 1.0)]);
    assert_eq!(calls_named(&g, outer, "helper"), vec![(nested, 1.0)]);
}

/// Stands for (R-F4): a fn in a two-level module keeps the innermost module
/// as its owner (the uid main computed), is unscoped, is defined by its
/// module, and a bare call from another file binds it through the global
/// tier. HEAD stored the owner `a::b`.
#[test]
fn test_build_rust_two_level_module_fn_owned_by_inner_module_global() {
    let src = "mod a {\n    pub mod b {\n        pub fn f() {}\n    }\n}\n";
    let caller = ("caller.rs", "fn g() {\n    f();\n}\n");
    let g = build(&[(PATH, src), caller]);
    let (b, f, call_site) = (
        node(&g, PATH, "b", 2),
        node(&g, PATH, "f", 3),
        node(&g, "caller.rs", "g", 1),
    );
    let bytes = archive(&g);
    let archived = access(&bytes);
    assert_eq!(owner(&g, f), "b");
    assert_eq!(
        g.nodes[f as usize].uid,
        uid_of(NodeKind::Function, Some("b"), "f")
    );
    assert!(!archived.is_lexically_scoped(f));
    assert!(has_edge(&g, RelType::Defines, b, f));
    let targets: Vec<u32> = calls_named(&g, call_site, "f")
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    assert_eq!(targets, vec![f]);
}

/// Stands for: in `mod tests`, a bare `helper()` from `t` and from a closure
/// inside `t` binds `tests::helper`, never the file-level `helper`. The
/// closure reads the name tiers through its root unscoped ancestor `t`,
/// whose owner is `tests`.
#[test]
fn test_build_rust_mod_tests_helper_preferred_over_file_level() {
    let src = "fn helper() -> i32 { 1 }\n\nmod tests {\n    fn helper() -> i32 { 2 }\n    fn t() -> i32 {\n        let xs = vec![1];\n        let _: Vec<i32> = xs.iter().map(|x| helper() + x).collect();\n        helper()\n    }\n}\n";
    let g = build(&[(PATH, src)]);
    let (file_level, tests_helper, t) = (
        node(&g, PATH, "helper", 1),
        node(&g, PATH, "helper", 4),
        node(&g, PATH, "t", 5),
    );
    let pool = g.string_pool.as_slice();
    let closure = (0..g.nodes.len() as u32)
        .find(|&i| {
            g.nodes[i as usize]
                .name
                .resolve(pool)
                .starts_with("<anonymous:7:")
        })
        .unwrap_or_else(|| panic!("setup: the closure on line 7 is a node"));
    assert_eq!(owner(&g, tests_helper), "tests");
    let bound = |source| -> Vec<u32> {
        calls_named(&g, source, "helper")
            .into_iter()
            .map(|(t, _)| t)
            .collect()
    };
    assert_eq!(bound(t), vec![tests_helper]);
    assert_eq!(bound(closure), vec![tests_helper]);
    assert!(!has_edge_into(&g, RelType::Calls, file_level));
}

/// Stands for: a bare name that passes the nested fn as a value
/// (`for_each(helper)`) references the nested fn, as a bare call binds it;
/// a method-syntax call `x.helper()` never binds a nested fn, which no
/// receiver can reach.
#[test]
fn test_build_rust_name_ref_binds_nested_method_syntax_does_not() {
    let src = "fn helper(_: &i32) {}\n\nfn outer() {\n    fn helper(_: &i32) {}\n    let xs = vec![1];\n    xs.iter().for_each(helper);\n}\n";
    let g = build(&[(PATH, src)]);
    let (top, outer, nested) = (
        node(&g, PATH, "helper", 1),
        node(&g, PATH, "outer", 3),
        node(&g, PATH, "helper", 4),
    );
    assert!(has_edge(&g, RelType::References, outer, nested));
    assert!(!has_edge(&g, RelType::References, outer, top));

    let src = "struct S;\nimpl S { fn helper(&self) {} }\nfn outer(x: S) {\n    fn helper() {}\n    x.helper();\n}\n";
    let g = build(&[(PATH, src)]);
    let (outer, nested) = (node(&g, PATH, "outer", 3), node(&g, PATH, "helper", 4));
    assert!(!has_edge(&g, RelType::Calls, outer, nested));
}
