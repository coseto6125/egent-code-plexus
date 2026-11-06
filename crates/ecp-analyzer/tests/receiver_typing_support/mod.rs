//! Shared harness for the `<lang>_receiver_typing.rs` fixtures.
//!
//! Every fixture has the same shape: a base type declares `greet`, a derived
//! type in another file inherits it, an unrelated decoy type declares its own
//! `greet`, and a caller types a receiver as the derived type with the
//! language's existing binder. The bare name `greet` is ambiguous (base and
//! decoy), so only the receiver-typing ladder can produce the edge.
#![allow(dead_code)]

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{CallSite, LocalGraph, RawNode};
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

pub fn parse_all<P: LanguageProvider>(provider: &P, files: &[(&str, &str)]) -> Vec<LocalGraph> {
    files
        .iter()
        .map(|(path, src)| {
            provider
                .parse_file(Path::new(path), src.as_bytes())
                .unwrap_or_else(|e| panic!("parse {path}: {e}"))
        })
        .collect()
}

/// Panics unless the binder attached `callee` to the node named `caller`.
/// A failure here is a binder gap, not a resolver gap.
pub fn assert_binder_emits(graphs: &[LocalGraph], caller: &str, callee: &str) {
    let calls: Vec<&str> = graphs
        .iter()
        .flat_map(|g| &g.nodes)
        .filter(|n| n.name == caller)
        .flat_map(|n| n.calls.iter().map(String::as_str))
        .collect();
    // A C# entry carries its argument record; compare the decoded sites.
    assert!(
        calls
            .iter()
            .any(|&raw| CallSite::parse(raw) == CallSite::parse(callee)),
        "binder: `{caller}` should call `{callee}`; calls: {calls:?}"
    );
}

/// The decoded call sites `provider` records in `source` (read as `path`)
/// for the node named `caller`, as `{:?}` text, sorted.
pub fn call_sites<P: LanguageProvider>(
    provider: &P,
    path: &str,
    source: &str,
    caller: &str,
) -> Vec<String> {
    let local = provider
        .parse_file(Path::new(path), source.as_bytes())
        .unwrap_or_else(|e| panic!("parse {path}: {e}"));
    let mut sites: Vec<String> = local
        .nodes
        .iter()
        .filter(|n| n.name == caller)
        .flat_map(|n| {
            n.calls
                .iter()
                .map(|raw| format!("{:?}", CallSite::parse(raw)))
        })
        .collect();
    sites.sort();
    sites
}

pub fn build(graphs: Vec<LocalGraph>) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for g in graphs {
        builder.add_graph(g);
    }
    builder.build()
}

/// Target file of every `Calls` edge from a node named `caller` to a node
/// named `callee`, one entry per edge.
pub fn callee_files(graph: &ZeroCopyGraph, caller: &str, callee: &str) -> Vec<String> {
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

/// `(target file, target name, reason, confidence)` of every `Calls` edge out
/// of a node named `caller`, sorted. The full row, so a test tells a typed
/// `call` from the tagged guess `call:untyped-member`.
pub fn call_rows(graph: &ZeroCopyGraph, caller: &str) -> Vec<(String, String, String, f32)> {
    let pool = graph.string_pool.as_slice();
    let mut rows: Vec<_> = graph
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
            (
                file,
                target.name.resolve(pool).to_string(),
                e.reason.resolve(pool).to_string(),
                e.confidence,
            )
        })
        .collect();
    rows.sort_by(|a, b| a.partial_cmp(b).expect("a confidence is never NaN"));
    rows
}

/// The rows of [`call_rows`] that name the method `method`.
pub fn call_rows_to(
    graph: &ZeroCopyGraph,
    caller: &str,
    method: &str,
) -> Vec<(String, String, String, f32)> {
    call_rows(graph, caller)
        .into_iter()
        .filter(|row| row.1 == method)
        .collect()
}

/// The shared scenario: the binder types the receiver (`typed_callee`), and
/// the graph holds exactly one `Calls` edge from `caller` to `method`, into
/// `base_file` (not the decoy).
pub fn assert_single_call_into_base(
    graphs: Vec<LocalGraph>,
    caller: &str,
    typed_callee: &str,
    method: &str,
    base_file: &str,
) {
    assert_binder_emits(&graphs, caller, typed_callee);
    let graph = build(graphs);
    assert_eq!(
        callee_files(&graph, caller, method),
        vec![base_file.to_string()],
        "`{typed_callee}` must resolve to the inherited method in {base_file}, once"
    );
}

// ---- Rule U: a member call on an untyped receiver ----------------------------
//
// The parsers' own emission of the untyped site is pinned by
// `receiver_emission.rs`. These graphs are hand-built so one fixture serves
// all 14 languages: the file extension picks the language, and the resolver
// reads the `UntypedMember` site the same way whichever parser wrote it.

/// The confidence and `reason` of a bind by the member name alone in a
/// guess language (Rust, Go, Java, Swift, Dart, C++, TypeScript, PHP, Python,
/// Ruby, JavaScript).
pub const UNTYPED_GUESS: (f32, &str) = (0.55, "call:untyped-member");

/// Where the untyped member call and the methods it may reach sit.
#[derive(Clone, Copy, Debug)]
pub enum UntypedLayout {
    /// One method `retain` on `Dirty`, in another file.
    OtherFile,
    /// The caller, a free function, and the one method share a file.
    SameFile,
    /// `retain` on two unrelated types, in two files.
    TwoMethods,
    /// The caller is a method of `Dirty`, the owner of the only `retain`.
    OwnType,
    /// `Dirty.retain` twice, in `core/pkg/` and in a duplicated tree
    /// `android/core/pkg/`: the copy nearest the caller binds.
    NearestCopy,
}

fn untyped_node(
    name: &str,
    kind: NodeKind,
    row: u32,
    owner: Option<&str>,
    calls: &[String],
) -> RawNode {
    RawNode {
        name: name.into(),
        kind,
        span: (row, 0, row + 2, 0),
        is_exported: false,
        heritage: vec![],
        type_annotation: None,
        decorators: vec![],
        calls: calls.to_vec(),
        field_reads: Vec::new(),
        owner_class: owner.map(str::to_string),
        content_hash: 0,
    }
}

fn untyped_file(path: String, nodes: Vec<RawNode>) -> LocalGraph {
    LocalGraph {
        file_path: path.into(),
        content_hash: [1; 8],
        nodes,
        ..Default::default()
    }
}

/// The type `owner` and its method `retain`, from row `row`.
fn retain_on(owner: &str, row: u32) -> Vec<RawNode> {
    vec![
        untyped_node(owner, NodeKind::Class, row, None, &[]),
        untyped_node("retain", NodeKind::Method, row + 3, Some(owner), &[]),
    ]
}

/// `(confidence, reason)` of every `Calls` edge the caller of `layout`
/// (`user`, or `caller` for [`UntypedLayout::OwnType`]) holds onto `retain`,
/// in a project whose files carry the extension `ext`.
pub fn untyped_member_edges(ext: &str, layout: UntypedLayout) -> Vec<(f32, String)> {
    let untyped = [CallSite::untyped_member("retain")];
    let path = |stem: &str| {
        let dir = if stem.contains('/') { "" } else { "src/" };
        format!("{dir}{stem}.{ext}")
    };
    let user = |row| untyped_node("user", NodeKind::Function, row, None, &untyped);
    let (graphs, caller) = match layout {
        UntypedLayout::OtherFile => (
            vec![
                untyped_file(path("def"), retain_on("Dirty", 1)),
                untyped_file(path("use"), vec![user(1)]),
            ],
            "user",
        ),
        UntypedLayout::SameFile => {
            let mut nodes = retain_on("Dirty", 1);
            nodes.push(user(10));
            (vec![untyped_file(path("one"), nodes)], "user")
        }
        UntypedLayout::TwoMethods => (
            vec![
                untyped_file(path("def"), retain_on("Dirty", 1)),
                untyped_file(path("other"), retain_on("Other", 1)),
                untyped_file(path("use"), vec![user(1)]),
            ],
            "user",
        ),
        UntypedLayout::NearestCopy => (
            vec![
                untyped_file(path("core/pkg/def"), retain_on("Dirty", 1)),
                untyped_file(path("android/core/pkg/def"), retain_on("Dirty", 1)),
                untyped_file(path("core/pkg/use"), vec![user(1)]),
            ],
            "user",
        ),
        UntypedLayout::OwnType => {
            let mut nodes = retain_on("Dirty", 1);
            nodes.push(untyped_node(
                "caller",
                NodeKind::Method,
                10,
                Some("Dirty"),
                &untyped,
            ));
            (vec![untyped_file(path("def"), nodes)], "caller")
        }
    };
    let graph = build(graphs);
    let pool = graph.string_pool.as_slice();
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == "retain")
        .map(|e| (e.confidence, e.reason.resolve(pool).to_string()))
        .collect()
}

/// A guess language: the bind by name alone is `UNTYPED_GUESS`, whichever
/// tier found the method (the other file's unique `retain` was Global, 0.7;
/// the same file's was SameFile, 1.0); an ambiguous name and the caller's own
/// type still bind nothing; the nearest copy of a duplicated declaration
/// keeps its own tag, 0.6 `call:nearest-copy`.
pub fn assert_untyped_member_is_guess(ext: &str) {
    let guess = vec![(UNTYPED_GUESS.0, UNTYPED_GUESS.1.to_string())];
    assert_eq!(
        untyped_member_edges(ext, UntypedLayout::OtherFile),
        guess,
        "{ext}: other file"
    );
    assert_eq!(
        untyped_member_edges(ext, UntypedLayout::SameFile),
        guess,
        "{ext}: same file"
    );
    assert!(
        untyped_member_edges(ext, UntypedLayout::TwoMethods).is_empty(),
        "{ext}: ambiguous"
    );
    assert!(
        untyped_member_edges(ext, UntypedLayout::OwnType).is_empty(),
        "{ext}: own type"
    );
    assert_eq!(
        untyped_member_edges(ext, UntypedLayout::NearestCopy),
        vec![(0.6, "call:nearest-copy".to_string())],
        "{ext}: nearest copy"
    );
}

/// A language the rule never touches (Kotlin, C): the bind keeps the
/// confidence of the tier that found the method, and the plain reason.
pub fn assert_untyped_member_keeps_tier_confidence(ext: &str) {
    let call = |confidence| vec![(confidence, "call".to_string())];
    assert_eq!(
        untyped_member_edges(ext, UntypedLayout::OtherFile),
        call(0.7),
        "{ext}: other file"
    );
    assert_eq!(
        untyped_member_edges(ext, UntypedLayout::SameFile),
        call(1.0),
        "{ext}: same file"
    );
    assert!(
        untyped_member_edges(ext, UntypedLayout::TwoMethods).is_empty(),
        "{ext}: ambiguous"
    );
    assert!(
        untyped_member_edges(ext, UntypedLayout::OwnType).is_empty(),
        "{ext}: own type"
    );
}

/// C#: a value of unknown type reaches no method by its name alone, in any
/// layout.
pub fn assert_untyped_member_binds_nothing(ext: &str) {
    for layout in [
        UntypedLayout::OtherFile,
        UntypedLayout::SameFile,
        UntypedLayout::TwoMethods,
        UntypedLayout::OwnType,
        UntypedLayout::NearestCopy,
    ] {
        assert!(
            untyped_member_edges(ext, layout).is_empty(),
            "{ext}: {layout:?}"
        );
    }
}
