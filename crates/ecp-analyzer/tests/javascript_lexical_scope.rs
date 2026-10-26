//! JavaScript function-local functions are lexically scoped (design A,
//! slice R8): `lexical_scope_kinds(JavaScript)` is `[Function]` in
//! `ScopedAndFileLevel` mode, so a nested function, an arrow function a
//! local `const` holds, or a callback node whose innermost container is a
//! callable has no owner, a lexical-parent row, and a uid hashed over its
//! parent's anchor. Only the lexical tier binds it (I5); a file-level
//! function stays bindable from the rest of the file and from importers.
//!
//! Every assertion reads what a consumer reads: the stored owner and uid,
//! the archived lexical-parent table and display owner, and the edges, on
//! the full build and on the overlay view of the same files.

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::identity::scoped_uid;
use ecp_core::graph::{ArchivedZeroCopyGraph, Edge, NodeKind, RelType, ZeroCopyGraph};
use ecp_core::session::{MergedGraph, OverlayFileInput, OverlayView};
use std::path::Path;

const PATH: &str = "test.js";
const SRC: &str = r#"function helper() { return 1; }

function outer() {
  function helper() { return 2; }
  return helper();
}

function other() {
  return helper();
}
"#;

/// The same shape with arrow functions held by `const`.
const ARROWS: &str = r#"const helper = () => 1;

function outer() {
  const helper = () => 2;
  return helper();
}

function other() {
  return helper();
}
"#;

/// Class `A`: `m` and `n` each nest their own `helper`.
const METHODS: &str = r#"class A {
  m() {
    function helper() { return 1; }
    return helper();
  }

  n() {
    function helper() { return 2; }
    return helper();
  }
}
"#;

/// Two levels of nesting, a callback node, and a nested function handed
/// on by name.
const DEEP: &str = r#"function outer(items) {
  function inner() {
    function deep() { return 1; }
    return deep();
  }
  function update(v) { return v; }
  items.forEach((item) => { inner(); });
  return items.map(update);
}
"#;

fn parse_at(path: &str, src: &str) -> LocalGraph {
    JavaScriptProvider::new()
        .expect("JavaScriptProvider::new")
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

fn uid_at(path: &str, kind: NodeKind, owner: Option<&str>, name: &str) -> u64 {
    ecp_core::uid::compute(kind, path, owner, name)
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

/// What the overlay reports for a name: every `Calls` edge as `(caller,
/// callee, callee display owner)`, and the display owner of each virtual node
/// of that name.
type CallsAndOwners = (Vec<(String, String, String)>, Vec<Option<String>>);

/// The overlay view of `files` over their own full build, every file dirty
/// with unchanged content: `(caller name, callee name, callee display
/// owner)` of every `Calls` edge, sorted, plus the display owner of each
/// virtual node named `name`, sorted.
fn overlay_calls_and_owners(files: &[(&str, &str)], name: &str) -> CallsAndOwners {
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
/// `outer`'s anchor, in every extension the JS provider parses; the
/// file-level `helper` keeps `compute(…, None, …)`. HEAD stored the owner
/// `outer` in the uid instead (C1: a scoped declaration's key is its
/// parent's anchor, not an owner).
#[test]
fn test_build_javascript_nested_fn_same_name_as_top_level_scoped_uid() {
    for path in [PATH, "test.jsx", "test.mjs", "test.cjs"] {
        let g = build(&[(path, SRC)]);
        let (top, outer, nested) = (
            node(&g, path, "helper", 1),
            node(&g, path, "outer", 3),
            node(&g, path, "helper", 4),
        );
        assert_eq!(
            g.nodes[top as usize].uid,
            uid_at(path, NodeKind::Function, None, "helper"),
            "{path}"
        );
        let outer_anchor = uid_at(path, NodeKind::Function, None, "outer");
        assert_eq!(g.nodes[outer as usize].uid, outer_anchor, "{path}");
        assert_eq!(
            g.nodes[nested as usize].uid,
            scoped_uid(outer_anchor, NodeKind::Function, "helper"),
            "{path}"
        );
        assert_ne!(
            g.nodes[nested as usize].uid,
            uid_at(path, NodeKind::Function, Some("outer"), "helper"),
            "{path}"
        );
        assert_eq!(owner(&g, nested), "", "{path}");
    }
}

/// Stands for: the parser stores no owner on the nested function (I1), and
/// the graph places it by its lexical-parent row; readers print `outer`
/// from the display owner, the text HEAD stored (C1 Qualifier match:
/// `outer.helper` names it).
#[test]
fn test_parse_file_javascript_nested_fn_owner_none_lexical_parent_outer() {
    let local = parse_at(PATH, SRC);
    let mut helpers: Vec<_> = local.nodes.iter().filter(|n| n.name == "helper").collect();
    helpers.sort_by_key(|n| n.span.0);
    assert_eq!(helpers.len(), 2);
    assert_eq!(helpers[0].owner_class, None);
    assert_eq!(helpers[1].owner_class, None);

    let g = build(&[(PATH, SRC)]);
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
/// the lexical tier at 1.0, and `other` binds the file-level one
/// (`ScopedAndFileLevel`; C1 Bare name: both stay addressable). The scoped
/// node gets no `Defines` edge. HEAD bound the same targets, but the nested
/// one carried the owner `outer`.
#[test]
fn test_build_javascript_call_to_bare_helper_binds_nested_other_binds_file_level() {
    for src in [SRC, ARROWS] {
        let g = build(&[(PATH, src)]);
        let (top, outer, nested, other) = (
            node(&g, PATH, "helper", 1),
            node(&g, PATH, "outer", 3),
            node(&g, PATH, "helper", 4),
            node(&g, PATH, "other", 8),
        );
        assert_eq!(g.nodes[nested as usize].kind, NodeKind::Function, "{src}");
        assert_eq!(
            calls_named(&g, outer, "helper"),
            vec![(nested, 1.0)],
            "{src}"
        );
        assert_eq!(calls_named(&g, other, "helper"), vec![(top, 1.0)], "{src}");
        assert_eq!(owner(&g, nested), "", "{src}");
        let bytes = archive(&g);
        assert_eq!(access(&bytes).lexical_parent(nested), Some(outer), "{src}");
        assert!(!has_edge_into(&g, RelType::Defines, nested), "{src}");
        assert!(has_edge_into(&g, RelType::Defines, top), "{src}");
    }
}

/// Stands for: an importer binds the exported file-level `helper`, never
/// the `helper` nested in `outer` (I5).
#[test]
fn test_build_javascript_importer_binds_file_level_never_nested() {
    let a = (
        "src/a.js",
        "export function helper() { return 1; }\n\nexport function outer() {\n  function helper() { return 2; }\n  return helper();\n}\n",
    );
    let b = (
        "src/b.js",
        "import { helper } from './a';\n\nexport function run() {\n  return helper();\n}\n",
    );
    let g = build(&[a, b]);
    assert_eq!(
        targets(calls_named(&g, node(&g, "src/b.js", "run", 3), "helper")),
        vec![node(&g, "src/a.js", "helper", 1)]
    );
}

/// Stands for: the `helper` nested in `A.m` and the one in `A.n` are two
/// nodes with two scoped uids, members of nothing: no `A -HasMethod->` edge
/// to either. Each method's bare call binds its own; the display owner
/// prints `A::m` / `A::n`, the text HEAD stored as their owner.
#[test]
fn test_build_javascript_nested_fn_in_two_methods_two_nodes_no_has_method() {
    let g = build(&[(PATH, METHODS)]);
    let bytes = archive(&g);
    let archived = access(&bytes);
    let (class, m, in_m, n, in_n) = (
        node(&g, PATH, "A", 1),
        node(&g, PATH, "m", 2),
        node(&g, PATH, "helper", 3),
        node(&g, PATH, "n", 7),
        node(&g, PATH, "helper", 8),
    );
    let anchor = |name| uid_at(PATH, NodeKind::Method, Some("A"), name);
    assert_eq!(
        g.nodes[m as usize].uid,
        anchor("m"),
        "setup: `m` is a method"
    );
    for (nested, parent, display, method) in [(in_m, m, "A::m", "m"), (in_n, n, "A::n", "n")] {
        assert_eq!(
            g.nodes[nested as usize].uid,
            scoped_uid(anchor(method), NodeKind::Function, "helper")
        );
        assert_eq!(owner(&g, nested), "");
        assert_eq!(archived.lexical_parent(nested), Some(parent));
        assert_eq!(archived.display_owner(nested).as_deref(), Some(display));
        assert!(!has_edge_into(&g, RelType::HasMethod, nested));
        assert!(has_edge(&g, RelType::HasMethod, class, parent));
        assert_eq!(calls_named(&g, parent, "helper"), vec![(nested, 1.0)]);
    }
}

/// Stands for: two levels of nesting print the whole parent chain
/// (`outer::inner`), a callback node is scoped under `outer` and its bare
/// `inner()` binds the nested `inner`, and `items.map(update)` references
/// the nested `update` through the lexical tier (the name tiers never
/// return a scoped node, I5).
#[test]
fn test_build_javascript_two_levels_and_callback_scoped() {
    let g = build(&[(PATH, DEEP)]);
    let bytes = archive(&g);
    let archived = access(&bytes);
    let (outer, inner, deep, update) = (
        node(&g, PATH, "outer", 1),
        node(&g, PATH, "inner", 2),
        node(&g, PATH, "deep", 3),
        node(&g, PATH, "update", 6),
    );
    let callback = named_like(&g, "<anonymous:");
    for scoped in [inner, deep, update, callback] {
        assert_eq!(owner(&g, scoped), "");
    }
    assert_eq!(archived.lexical_parent(deep), Some(inner));
    assert_eq!(
        archived.display_owner(deep).as_deref(),
        Some("outer::inner")
    );
    assert_eq!(archived.lexical_parent(callback), Some(outer));
    let inner_uid = scoped_uid(
        uid_at(PATH, NodeKind::Function, None, "outer"),
        NodeKind::Function,
        "inner",
    );
    assert_eq!(g.nodes[inner as usize].uid, inner_uid);
    assert_eq!(
        g.nodes[deep as usize].uid,
        scoped_uid(inner_uid, NodeKind::Function, "deep")
    );
    assert_eq!(calls_named(&g, inner, "deep"), vec![(deep, 1.0)]);
    assert_eq!(calls_named(&g, callback, "inner"), vec![(inner, 1.0)]);
    assert!(has_edge(&g, RelType::References, outer, update));
}

/// Stands for: an empty file and a file of file-level functions scope
/// nothing and write no lexical-parent row.
#[test]
fn test_build_javascript_empty_and_flat_file_no_lexical_rows() {
    for src in [
        "",
        "function a() { return b(); }\nfunction b() { return 1; }\n",
    ] {
        let g = build(&[(PATH, src)]);
        let bytes = archive(&g);
        assert!(access(&bytes).lexical_parents.is_empty(), "{src:?}");
    }
}

/// Stands for: the overlay view of a dirty JavaScript file binds and prints
/// what the full build does, two same-named nested functions included
/// (FU-2026-10-08-92c784f892b3: the overlay once dropped them as one
/// ambiguous name).
#[test]
fn test_overlay_view_javascript_nested_fns_match_full_build() {
    let files = [(PATH, METHODS)];
    let (calls, owners) = overlay_calls_and_owners(&files, "helper");
    assert_eq!(calls, full_calls(&files));
    assert_eq!(owners, [Some("A::m".to_string()), Some("A::n".to_string())]);
    let deep = [(PATH, DEEP)];
    let (calls, owners) = overlay_calls_and_owners(&deep, "deep");
    assert_eq!(calls, full_calls(&deep));
    assert_eq!(owners, [Some("outer::inner".to_string())]);
    let src = [(PATH, SRC)];
    let (calls, _) = overlay_calls_and_owners(&src, "helper");
    assert_eq!(calls, full_calls(&src));
}
