//! Python function-local functions are lexically scoped (design A, slice
//! R8): `lexical_scope_kinds(Python)` is `[Function]` in `ScopedOnly` mode,
//! so a `def` or lambda node whose innermost container is a callable has no
//! owner, a lexical-parent row, and a uid hashed over its parent's anchor.
//! Only the lexical tier binds it (I5); a module-level function stays with
//! the name tiers. Non-callables (a local class, a variable) keep the
//! enclosing function's name as their owner, and their uid.
//!
//! Every assertion reads what a consumer reads: the stored owner and uid,
//! the archived lexical-parent table and display owner, and the edges, on
//! the full build and on the overlay view of the same files.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::identity::scoped_uid;
use ecp_core::graph::{ArchivedZeroCopyGraph, Edge, NodeKind, RelType, ZeroCopyGraph};
use ecp_core::session::{MergedGraph, OverlayFileInput, OverlayView};
use std::path::Path;

const PATH: &str = "test.py";
const SRC: &str = r#"def helper():
    return 1


def outer():
    def helper():
        return 2
    return helper()


def other():
    return helper()
"#;

/// Class `A`: `m` and `n` each nest their own `helper`; `k` nests `n`
/// (named like the method `A.n`) and calls the module-level `helper`.
const METHODS: &str = r#"def helper():
    return 0


class A:
    def m(self):
        def helper():
            return 1
        return helper()

    def n(self):
        def helper():
            return 2
        return helper()

    def k(self):
        def n():
            return 3
        return helper() + n()
"#;

/// Two levels of nesting, a decorated nested def, a lambda node and a
/// nested def handed on by name.
const DEEP: &str = r#"import functools


def outer(f):
    def inner():
        def deep():
            return 1
        return deep()

    @functools.wraps(f)
    def wrapper(*args):
        return list(map(lambda x: inner(), args))

    return run(wrapper)
"#;

/// Two test functions each declare a local `class Fake`.
const LOCAL_CLASSES: &str = r#"def t1():
    class Fake:
        pass
    return Fake()


def t2():
    class Fake:
        pass
    return Fake()
"#;

fn parse_at(path: &str, src: &str) -> LocalGraph {
    PythonProvider::new()
        .expect("PythonProvider::new")
        .parse_file(Path::new(path), src.as_bytes())
        .expect("parse_file")
}

fn build(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for &(path, src) in files {
        builder.add_graph(parse_at(path, src));
    }
    builder.build()
}

fn archive(g: &ZeroCopyGraph) -> rkyv::util::AlignedVec {
    rkyv::to_bytes::<rkyv::rancor::Error>(g).expect("archive graph")
}

fn access(bytes: &[u8]) -> &ArchivedZeroCopyGraph {
    rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).expect("access graph")
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

/// The index of the one Function whose name starts with `prefix`. A
/// `Process` node takes its entry function's name, so it is left out.
fn named_like(g: &ZeroCopyGraph, prefix: &str) -> u32 {
    let pool = g.string_pool.as_slice();
    let hits: Vec<u32> = (0..g.nodes.len() as u32)
        .filter(|&i| {
            let n = &g.nodes[i as usize];
            n.kind == NodeKind::Function && n.name.resolve(pool).starts_with(prefix)
        })
        .collect();
    assert_eq!(hits.len(), 1, "setup: one Function named `{prefix}…`");
    hits[0]
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

fn targets(calls: Vec<(u32, f32)>) -> Vec<u32> {
    calls.into_iter().map(|(t, _)| t).collect()
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

fn blind_spot_kinds(g: &ZeroCopyGraph) -> Vec<String> {
    let pool = g.string_pool.as_slice();
    g.blind_spots
        .iter()
        .map(|b| b.kind.resolve(pool).to_string())
        .collect()
}

/// The overlay view of `files` over their own full build, every file dirty
/// with unchanged content: `(caller name, callee name, callee display
/// owner)` of every `Calls` edge, sorted, plus the display owner of each
/// virtual node named `name`, sorted.
fn overlay_calls_and_owners(
    files: &[(&str, &str)],
    name: &str,
) -> (Vec<(String, String, String)>, Vec<Option<String>>) {
    let full = build(files);
    let bytes = archive(&full);
    let base = access(&bytes);
    let dirty: Vec<OverlayFileInput> = files
        .iter()
        .map(|&(path, src)| OverlayFileInput::from(parse_at(path, src)))
        .collect();
    let view = OverlayView::build(base, &dirty).expect("a dirty view");
    let merged = MergedGraph::new(base, Some(&view));
    let display = |idx: u32| merged.display_owner(idx).map(|o| o.into_owned());
    let mut calls: Vec<(String, String, String)> = merged
        .all_edges()
        .filter(|edge| edge.rel_type() == RelType::Calls)
        .map(|edge| {
            let source = merged.node(edge.source).unwrap();
            let target = merged.node(edge.target).unwrap();
            (
                source.name(&merged).to_string(),
                target.name(&merged).to_string(),
                display(edge.target).unwrap_or_default(),
            )
        })
        .collect();
    calls.sort();
    let mut owners: Vec<Option<String>> = view
        .virtual_nodes()
        .iter()
        .enumerate()
        .filter(|(_, n)| n.name == name)
        .map(|(at, _)| display(view.base_len() + at as u32))
        .collect();
    owners.sort();
    (calls, owners)
}

/// The same triples as [`overlay_calls_and_owners`], read off the full
/// build through the archived display owner.
fn full_calls(files: &[(&str, &str)]) -> Vec<(String, String, String)> {
    let g = build(files);
    let bytes = archive(&g);
    let archived = access(&bytes);
    let pool = g.string_pool.as_slice();
    let mut calls: Vec<(String, String, String)> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .map(|e| {
            (
                g.nodes[e.source as usize].name.resolve(pool).to_string(),
                g.nodes[e.target as usize].name.resolve(pool).to_string(),
                archived
                    .display_owner(e.target)
                    .map(|o| o.into_owned())
                    .unwrap_or_default(),
            )
        })
        .collect();
    calls.sort();
    calls
}

/// Stands for: the nested `helper` is a scoped node, its uid hashed over
/// `outer`'s anchor; the module-level `helper` keeps `compute(…, None, …)`.
/// HEAD stored the owner `outer` in the uid instead (C1: a scoped
/// declaration's key is its parent's anchor, not an owner).
#[test]
fn test_build_python_nested_fn_same_name_as_top_level_scoped_uid() {
    let g = build(&[(PATH, SRC)]);
    let (top, outer, nested) = (
        node(&g, PATH, "helper", 1),
        node(&g, PATH, "outer", 5),
        node(&g, PATH, "helper", 6),
    );
    assert_eq!(g.nodes[top as usize].kind, NodeKind::Function);
    assert_eq!(
        g.nodes[top as usize].uid,
        uid_of(NodeKind::Function, None, "helper")
    );
    let outer_anchor = uid_of(NodeKind::Function, None, "outer");
    assert_eq!(g.nodes[outer as usize].uid, outer_anchor);
    assert_eq!(
        g.nodes[nested as usize].uid,
        scoped_uid(outer_anchor, NodeKind::Function, "helper")
    );
    assert_ne!(
        g.nodes[nested as usize].uid,
        uid_of(NodeKind::Function, Some("outer"), "helper")
    );
    assert!(
        g.nodes
            .iter()
            .filter(|n| n.kind != NodeKind::File)
            .all(|n| !n.name.resolve(g.string_pool.as_slice()).is_empty()),
        "no tombstone"
    );
}

/// Stands for: the parser stores no owner on the nested def (I1), and the
/// graph places it by its lexical-parent row; readers print `outer` from the
/// display owner (C1 Qualifier match: `outer.helper` names it). HEAD stored
/// the owner `outer` and wrote no row.
#[test]
fn test_parse_file_python_nested_fn_owner_none_lexical_parent_outer() {
    let local = parse_at(PATH, SRC);
    let mut helpers: Vec<_> = local.nodes.iter().filter(|n| n.name == "helper").collect();
    helpers.sort_by_key(|n| n.span.0);
    assert_eq!(helpers.len(), 2);
    assert_eq!(helpers[0].owner_class, None);
    assert_eq!(helpers[1].owner_class, None);

    let g = build(&[(PATH, SRC)]);
    let (top, outer, nested) = (
        node(&g, PATH, "helper", 1),
        node(&g, PATH, "outer", 5),
        node(&g, PATH, "helper", 6),
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
/// the lexical tier at 1.0, and `other`, which nests no `helper`, binds the
/// module-level one (C1 Bare name: both declarations stay addressable).
/// HEAD bound `outer`'s call to the module-level `helper` although the
/// nested def shadows it.
#[test]
fn test_build_python_call_to_bare_helper_binds_nested_other_binds_module_level() {
    let g = build(&[(PATH, SRC)]);
    let (top, outer, nested, other) = (
        node(&g, PATH, "helper", 1),
        node(&g, PATH, "outer", 5),
        node(&g, PATH, "helper", 6),
        node(&g, PATH, "other", 11),
    );
    assert_eq!(calls_named(&g, outer, "helper"), vec![(nested, 1.0)]);
    assert_eq!(targets(calls_named(&g, other, "helper")), vec![top]);
    assert_eq!(owner(&g, nested), "");
    assert!(!has_edge_into(&g, RelType::HasMethod, nested));
    assert!(!has_edge_into(&g, RelType::Defines, nested));
    assert!(has_edge_into(&g, RelType::Defines, top));
}

/// Stands for: the `helper` nested in `A.m` and the one in `A.n` are two
/// nodes with two scoped uids, members of nothing: no `A -HasMethod->` edge
/// to either, where HEAD gave both the owner `A` (one uid, a tombstone)
/// and a `HasMethod` edge. Each method's bare call binds its own; `A.k`,
/// which nests no `helper`, binds the module-level one; the `n` nested in
/// `A.k` is no member either and `k`'s `n()` binds it, not the method.
#[test]
fn test_build_python_nested_fn_in_two_methods_two_nodes_no_has_method() {
    let g = build(&[(PATH, METHODS)]);
    let bytes = archive(&g);
    let archived = access(&bytes);
    let (top, class, m, in_m, n, in_n, k, n_in_k) = (
        node(&g, PATH, "helper", 1),
        node(&g, PATH, "A", 5),
        node(&g, PATH, "m", 6),
        node(&g, PATH, "helper", 7),
        node(&g, PATH, "n", 11),
        node(&g, PATH, "helper", 12),
        node(&g, PATH, "k", 16),
        node(&g, PATH, "n", 17),
    );
    let anchor = |name| uid_of(NodeKind::Method, Some("A"), name);
    assert_eq!(
        g.nodes[m as usize].uid,
        anchor("m"),
        "setup: `m` is a method"
    );
    assert_eq!(
        g.nodes[in_m as usize].uid,
        scoped_uid(anchor("m"), NodeKind::Function, "helper")
    );
    assert_eq!(
        g.nodes[in_n as usize].uid,
        scoped_uid(anchor("n"), NodeKind::Function, "helper")
    );
    assert_eq!(
        g.nodes[n_in_k as usize].uid,
        scoped_uid(anchor("k"), NodeKind::Function, "n")
    );
    for (nested, parent, display) in [(in_m, m, "A::m"), (in_n, n, "A::n"), (n_in_k, k, "A::k")] {
        assert_eq!(owner(&g, nested), "");
        assert_eq!(archived.lexical_parent(nested), Some(parent));
        assert_eq!(archived.display_owner(nested).as_deref(), Some(display));
        assert!(!has_edge_into(&g, RelType::HasMethod, nested));
        assert!(!has_edge_into(&g, RelType::Defines, nested));
    }
    for method in [m, n, k] {
        assert!(has_edge(&g, RelType::HasMethod, class, method));
        assert!(!archived.is_lexically_scoped(method));
    }
    assert_eq!(calls_named(&g, m, "helper"), vec![(in_m, 1.0)]);
    assert_eq!(calls_named(&g, n, "helper"), vec![(in_n, 1.0)]);
    assert_eq!(targets(calls_named(&g, k, "helper")), vec![top]);
    assert_eq!(calls_named(&g, k, "n"), vec![(n_in_k, 1.0)]);
    assert!(
        !blind_spot_kinds(&g).contains(&"uid-collision".to_string()),
        "{:?}",
        blind_spot_kinds(&g)
    );
}

/// Stands for: two levels of nesting print the whole parent chain
/// (`outer::inner`), a decorated nested def and a lambda node are scoped as
/// a plain one is, the lambda's bare `inner()` binds `outer`'s nested
/// `inner`, and `run(wrapper)` references the nested `wrapper` through the
/// lexical tier (the name tiers never return a scoped node, I5). HEAD
/// stored the owners `inner`, `outer` and `wrapper`.
#[test]
fn test_build_python_two_levels_decorated_and_lambda_scoped() {
    let g = build(&[(PATH, DEEP)]);
    let bytes = archive(&g);
    let archived = access(&bytes);
    let pool = g.string_pool.as_slice();
    let one = |name: &str| {
        let hits: Vec<u32> = (0..g.nodes.len() as u32)
            .filter(|&i| g.nodes[i as usize].name.resolve(pool) == name)
            .collect();
        assert_eq!(hits.len(), 1, "setup: one `{name}`");
        hits[0]
    };
    let (outer, inner, deep, wrapper) = (one("outer"), one("inner"), one("deep"), one("wrapper"));
    let lambda = named_like(&g, "<anonymous:");
    for scoped in [inner, deep, wrapper, lambda] {
        assert_eq!(owner(&g, scoped), "");
        assert!(!has_edge_into(&g, RelType::Defines, scoped));
    }
    assert_eq!(archived.lexical_parent(inner), Some(outer));
    assert_eq!(archived.lexical_parent(deep), Some(inner));
    assert_eq!(
        archived.display_owner(deep).as_deref(),
        Some("outer::inner")
    );
    assert_eq!(archived.lexical_parent(wrapper), Some(outer));
    assert_eq!(archived.lexical_parent(lambda), Some(wrapper));
    assert_eq!(
        archived.display_owner(lambda).as_deref(),
        Some("outer::wrapper")
    );
    let inner_uid = scoped_uid(
        uid_of(NodeKind::Function, None, "outer"),
        NodeKind::Function,
        "inner",
    );
    assert_eq!(g.nodes[inner as usize].uid, inner_uid);
    assert_eq!(
        g.nodes[deep as usize].uid,
        scoped_uid(inner_uid, NodeKind::Function, "deep")
    );
    assert_eq!(calls_named(&g, inner, "deep"), vec![(deep, 1.0)]);
    assert_eq!(calls_named(&g, lambda, "inner"), vec![(inner, 1.0)]);
    assert!(has_edge(&g, RelType::References, outer, wrapper));
}

/// Stands for: a local class is no scoped node. Each `class Fake` keeps the
/// enclosing function's name as its owner and today's uid, and the two do
/// not collide (design A §5.2, `stamp_owner_fn_by_span` skips only the
/// scoped kinds).
#[test]
fn test_parse_python_local_class_in_two_functions_keeps_two_uids() {
    let g = build(&[(PATH, LOCAL_CLASSES)]);
    let bytes = archive(&g);
    let archived = access(&bytes);
    let (first, second) = (node(&g, PATH, "Fake", 2), node(&g, PATH, "Fake", 8));
    assert_eq!(owner(&g, first), "t1");
    assert_eq!(owner(&g, second), "t2");
    assert_eq!(
        g.nodes[first as usize].uid,
        uid_of(NodeKind::Class, Some("t1"), "Fake")
    );
    assert_eq!(
        g.nodes[second as usize].uid,
        uid_of(NodeKind::Class, Some("t2"), "Fake")
    );
    assert!(!archived.is_lexically_scoped(first));
    assert!(!archived.is_lexically_scoped(second));
    assert!(archived.lexical_parents.is_empty());
    assert!(
        !blind_spot_kinds(&g).contains(&"uid-collision".to_string()),
        "{:?}",
        blind_spot_kinds(&g)
    );
}

/// Stands for: a nested def is invisible to the name tiers (I5). A bare
/// `helper()` in another module never binds the `helper` nested in
/// `a.outer`; with a module-level `helper` imported from `c` it binds that
/// one.
#[test]
fn test_build_python_nested_fn_other_file_bare_call_skips_scoped() {
    let a = (
        "a.py",
        "def outer():\n    def helper():\n        return 1\n    return helper()\n",
    );
    let b = ("b.py", "def b():\n    return helper()\n");
    let g = build(&[a, b]);
    assert_eq!(calls_named(&g, node(&g, "b.py", "b", 1), "helper"), vec![]);

    let b = (
        "b.py",
        "from c import helper\n\n\ndef b():\n    return helper()\n",
    );
    let c = ("c.py", "def helper():\n    return 3\n");
    let g = build(&[a, b, c]);
    assert_eq!(
        targets(calls_named(&g, node(&g, "b.py", "b", 4), "helper")),
        vec![node(&g, "c.py", "helper", 1)]
    );
}

/// Stands for: an empty file and a file of module-level defs scope nothing
/// and write no lexical-parent row.
#[test]
fn test_build_python_empty_and_flat_file_no_lexical_rows() {
    for src in ["", "def a():\n    return b()\n\n\ndef b():\n    return 1\n"] {
        let g = build(&[(PATH, src)]);
        let bytes = archive(&g);
        assert!(access(&bytes).lexical_parents.is_empty(), "{src:?}");
    }
}

/// Stands for: the overlay view of a dirty Python file binds and prints
/// what the full build does: each method's own nested `helper`, `A.k`'s
/// module-level `helper` and nested `n`, each nested node printed with its
/// parent chain. HEAD's overlay saw the owner `A` on both nested helpers.
#[test]
fn test_overlay_view_python_nested_fns_match_full_build() {
    let files = [(PATH, METHODS)];
    let (calls, owners) = overlay_calls_and_owners(&files, "helper");
    assert_eq!(calls, full_calls(&files));
    assert_eq!(
        owners,
        [None, Some("A::m".to_string()), Some("A::n".to_string())]
    );
    let deep = [(PATH, DEEP)];
    let (calls, owners) = overlay_calls_and_owners(&deep, "deep");
    assert_eq!(calls, full_calls(&deep));
    assert_eq!(owners, [Some("outer::inner".to_string())]);
}
