//! Per-file mode of the incremental overlay (architecture direction, "Beyond
//! calls: the incremental overlay"): the outputs a fresh build makes for one
//! file from that file's parse alone, replayed for each dirty file from its
//! fragment's records. Every endpoint comes from the file's own raw indexes
//! through a [`RawIndexMap`]; no name lookup leaves the file.
//!
//! | Output | Index derivation | Replay |
//! |---|---|---|
//! | `File` node | `GraphBuilder::build`, one per `LocalGraph` | one virtual node per input whose path has no entry in `graph.files`; a path that has one keeps its base `File` node |
//! | scope `Defines` | `post_process::scope_defines` | [`containment::scope_defines`] |
//! | enum containment | `post_process::enum_variant_defines` | [`containment::enum_variant_defines`] |
//! | function meta | `pass1_8_function_metas` | the flags and return type the fragment paired by span |
//!
//! [`OverlayView::build_with_removed`](super::view::OverlayView::build_with_removed)
//! calls [`replay`] once, after the virtual nodes and their tombstones are
//! final. Cost: O(dirty symbols) hash operations, plus one O(symbols of the
//! path) table for a path that declares an enum variant. Nothing scans the
//! base graph.

use super::view::{reason_index, OverlayFileInput, ViewEdge, ViewFunctionMeta, ViewNode};
use crate::analyzer::containment::{
    self, Row, Scope, ENUM_VARIANT_DEFINES_REASON, SCOPE_DEFINES_REASON,
};
use crate::analyzer::heritage::HeritageNode;
use crate::analyzer::types::owner_key;
use crate::contracts::raw_records::{FileOrd, RawIdx, RawIndexMap};
use crate::graph::{NodeKind, RelType};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

/// What the build hands the replay. Paths are `File.path`-normalized.
pub(super) struct ReplayInputs<'a> {
    pub files: &'a [OverlayFileInput],
    pub base_len: u32,
    /// `(tombstone, survivor)` virtual indexes: each later declaration the
    /// index tombstones, with the first of its uid.
    pub tombstones: Vec<(u32, u32)>,
    /// The dirty paths `graph.files` holds.
    pub indexed: FxHashSet<&'a str>,
    /// Each dirty path's last base `File` node, in graph order: the one the
    /// index's `file_node_idx` keeps, so the one its `Defines` start at.
    pub base_files: FxHashMap<&'a str, u32>,
    /// Scoped virtual index → its lexical parent ([`super::view`]'s table).
    pub lexical_parents: &'a FxHashMap<u32, u32>,
}

/// The replay's rows beside the nodes and edges it appends.
#[derive(Debug, Default)]
pub(super) struct Replayed {
    /// The base `File` nodes whose `Defines` the replay rebuilt: the view
    /// masks their base `Defines`.
    pub defines_files: FxHashSet<u32>,
    /// Ascending by node.
    pub function_metas: Vec<ViewFunctionMeta>,
}

/// Append the per-file outputs of `inputs.files`: virtual `File` nodes onto
/// `nodes`, and `Defines` edges onto `edges` with their adjacency. `nodes`
/// holds every virtual symbol node, tombstones applied. Returns nothing but
/// the function metas when the inputs break [`RawIndexMap`]'s invariants,
/// which `file_identities` output never does: no edge beats a wrong one.
pub(super) fn replay(
    inputs: ReplayInputs<'_>,
    nodes: &mut Vec<ViewNode>,
    edges: &mut Vec<ViewEdge>,
    out_adj: &mut FxHashMap<u32, Vec<u32>>,
    in_adj: &mut FxHashMap<u32, Vec<u32>>,
) -> Replayed {
    let ReplayInputs {
        files,
        base_len,
        tombstones,
        indexed,
        base_files,
        lexical_parents,
    } = inputs;
    let mut replayed = Replayed {
        function_metas: function_metas(files, base_len),
        ..Replayed::default()
    };
    let Some(raw_map) = RawIndexMap::new(base_len, files.iter().map(|f| f.symbols.len()))
        .and_then(|map| map.with_merged(tombstones))
        .ok()
    else {
        return replayed;
    };

    // One `File` node per input of a path the base never indexed, as the
    // index makes one per `LocalGraph`; the last one of a path is the one
    // its `file_node_idx` keeps.
    let mut new_files: FxHashMap<&str, u32> = FxHashMap::default();
    for file in files {
        let path = file.rel_path.as_str();
        if indexed.contains(path) {
            continue;
        }
        new_files.insert(path, base_len + nodes.len() as u32);
        nodes.push(file_node(path));
    }
    replayed.defines_files = base_files.values().copied().collect();

    let mut push = |edge: ViewEdge| {
        debug_assert!(
            reason_index(edge.reason).is_some(),
            "VIEW_EDGE_REASONS must list every edge reason, or the view is never cached"
        );
        let ei = edges.len() as u32;
        edges.push(edge);
        out_adj.entry(edge.source).or_default().push(ei);
        in_adj.entry(edge.target).or_default().push(ei);
    };
    let defines = |source: u32, target: u32, reason: &'static str| ViewEdge {
        source,
        target,
        rel_type: RelType::Defines,
        confidence: 1.0,
        reason,
    };

    // First declarations per path, built once for a path with a variant.
    let mut declarations: FxHashMap<&str, Declarations<'_>> = FxHashMap::default();
    for (ord, file) in files.iter().enumerate() {
        let path = file.rel_path.as_str();
        let ord = FileOrd(ord as u32);
        let node = |raw: usize| raw_map.node(ord, RawIdx(raw as u32));

        // A path with no `File` node (only a hand-made graph has one) gets
        // no scope `Defines`, as the index skips it.
        if let Some(&file_idx) = base_files.get(path).or_else(|| new_files.get(path)) {
            containment::scope_defines(&file.symbols, row, |scope, child| {
                let Some(child) = node(child).filter(|id| !lexical_parents.contains_key(id)) else {
                    return;
                };
                let source = match scope {
                    Scope::File => Some(file_idx),
                    Scope::Container(container) => node(container),
                };
                if let Some(source) = source {
                    push(defines(source, child, SCOPE_DEFINES_REASON));
                }
            });
        }

        if !file.symbols.iter().any(|s| s.kind == NodeKind::EnumVariant) {
            continue;
        }
        let table = declarations
            .entry(path)
            .or_insert_with(|| Declarations::of(files, path, &raw_map, nodes));
        containment::enum_variant_defines(
            &file.symbols,
            row,
            |name, kind, owner| table.first(name, kind, owner),
            |source, target| push(defines(source, target, ENUM_VARIANT_DEFINES_REASON)),
        );
    }
    replayed
}

fn row(symbol: &super::view::OverlaySymbol) -> Row<'_> {
    Row {
        kind: symbol.kind,
        owner: symbol.owner_class.as_deref(),
        name: &symbol.name,
        span: HeritageNode::span(symbol),
    }
}

/// The index's `File` node of `path`: the uid of `GraphBuilder::build`, the
/// base name for its name (the whole path when it has none), line 1.
fn file_node(path: &str) -> ViewNode {
    let name = std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned());
    ViewNode {
        uid: crate::uid::compute(NodeKind::File, path, None, path),
        name,
        kind: NodeKind::File,
        owner_class: None,
        rel_path: Arc::from(path),
        start_line: 1,
        end_line: 1,
        start_column: 0,
        end_column: 0,
        replaced_base: None,
        later_overload: false,
    }
}

/// The first declaration of each (name, kind, owner key) among one path's
/// Enum and EnumVariant nodes, in input order: the index's
/// `SymbolTable::first_declaration`, which registers neither a later
/// overload nor a tombstone.
struct Declarations<'a> {
    /// Keyed by (name, `NodeKind` discriminant, owner key).
    first: FxHashMap<(&'a str, u8, Option<&'a str>), u32>,
}

impl<'a> Declarations<'a> {
    fn of(
        files: &'a [OverlayFileInput],
        path: &str,
        raw_map: &RawIndexMap,
        nodes: &[ViewNode],
    ) -> Self {
        let mut first = FxHashMap::default();
        for (ord, file) in files.iter().enumerate() {
            if file.rel_path != path {
                continue;
            }
            let ord = FileOrd(ord as u32);
            for (raw, symbol) in file.symbols.iter().enumerate() {
                if !matches!(symbol.kind, NodeKind::Enum | NodeKind::EnumVariant) {
                    continue;
                }
                let raw = RawIdx(raw as u32);
                let (Some(slot), Some(node)) = (raw_map.slot(ord, raw), raw_map.node(ord, raw))
                else {
                    continue;
                };
                let registered =
                    slot == node && !nodes[(slot - raw_map.base()) as usize].later_overload;
                if registered {
                    let owner = symbol.owner_class.as_deref().and_then(owner_key);
                    first
                        .entry((symbol.name.as_str(), symbol.kind as u8, owner))
                        .or_insert(slot);
                }
            }
        }
        Self { first }
    }

    fn first(&self, name: &str, kind: NodeKind, owner: Option<&str>) -> Option<u32> {
        self.first
            .get(&(name, kind as u8, owner.and_then(owner_key)))
            .copied()
    }
}

/// The function meta of every virtual slot whose fragment paired one, as
/// pass 1.8 records it for the slot (a tombstone slot included).
fn function_metas(files: &[OverlayFileInput], base_len: u32) -> Vec<ViewFunctionMeta> {
    let mut out = Vec::new();
    let mut node = base_len;
    for symbol in files.iter().flat_map(|f| &f.symbols) {
        if let Some(flags) = symbol.flags {
            out.push(ViewFunctionMeta {
                node,
                flags,
                return_type: symbol.return_type.clone(),
            });
        }
        node += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{ArchivedZeroCopyGraph, FunctionMeta};
    use crate::graph_fixture::GraphFixture;
    use crate::session::merged::MergedGraph;
    use crate::session::view::{OverlaySymbol, OverlayView};
    use rkyv::rancor::Error as RkyvError;

    fn sym(kind: NodeKind, owner: Option<&str>, name: &str, lines: (u32, u32)) -> OverlaySymbol {
        OverlaySymbol {
            is_exported: false,
            name: name.to_string(),
            kind,
            owner_class: owner.map(str::to_string),
            start_line: lines.0,
            end_line: lines.1,
            start_column: 0,
            end_column: 0,
            calls: Vec::new(),
            heritage: Vec::new(),
            variant_refs: Vec::new(),
            return_type: None,
            generic_arity: 0,
            flags: None,
        }
    }

    fn func(name: &str, lines: (u32, u32)) -> OverlaySymbol {
        sym(NodeKind::Function, None, name, lines)
    }

    fn input(path: &str, symbols: Vec<OverlaySymbol>) -> OverlayFileInput {
        OverlayFileInput {
            rel_path: path.to_string(),
            symbols,
            imports: Vec::new(),
            export_aliases: Vec::new(),
        }
    }

    /// One clean file, so a virtual index never collides with base 0.
    fn clean_base() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        fx.func("src/clean.rs", "clean_fn");
        fx.into_bytes()
    }

    fn access(bytes: &[u8]) -> &ArchivedZeroCopyGraph {
        rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(bytes).unwrap()
    }

    /// Every `Defines` edge of the merged graph as (source, target, reason).
    fn defines(graph: &ArchivedZeroCopyGraph, view: &OverlayView) -> Vec<(u32, u32, String)> {
        let merged = MergedGraph::new(graph, Some(view));
        let mut out: Vec<(u32, u32, String)> = merged
            .all_edges()
            .filter(|e| e.rel_type() == RelType::Defines)
            .map(|e| (e.source, e.target, e.reason(graph).to_string()))
            .collect();
        out.sort();
        out
    }

    fn scope(source: u32, target: u32) -> (u32, u32, String) {
        (source, target, SCOPE_DEFINES_REASON.to_string())
    }

    /// The virtual `File` nodes of `view`, as (index, name, path).
    fn file_nodes(view: &OverlayView) -> Vec<(u32, String, String)> {
        (view.base_len()..)
            .zip(view.virtual_nodes())
            .filter(|(_, n)| n.kind == NodeKind::File)
            .map(|(idx, n)| (idx, n.name.clone(), n.rel_path.to_string()))
            .collect()
    }

    #[test]
    fn test_replay_new_file_gets_file_node_and_defines_edge_per_top_level_declaration() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/new.rs",
            vec![func("a", (1, 2)), func("b", (4, 5))],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();

        let file = base + 2;
        assert_eq!(
            file_nodes(&view),
            [(file, "new.rs".to_string(), "src/new.rs".to_string())]
        );
        let node = view.node(file).unwrap();
        assert_eq!(
            node.uid,
            crate::uid::compute(NodeKind::File, "src/new.rs", None, "src/new.rs"),
            "the index's File uid"
        );
        assert_eq!(
            (node.start_line, node.end_line, node.owner_class.as_deref()),
            (1, 1, None)
        );
        assert_eq!(
            defines(graph, &view),
            [scope(file, base), scope(file, base + 1)]
        );
        let edge = view.overlay_out(file).next().unwrap().1;
        assert_eq!(edge.confidence, 1.0);
    }

    #[test]
    fn test_replay_new_empty_file_has_file_node_and_no_defines() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let view = OverlayView::build(graph, &[input("src/empty.ts", Vec::new())]).unwrap();
        assert_eq!(
            file_nodes(&view),
            [(
                view.base_len(),
                "empty.ts".to_string(),
                "src/empty.ts".to_string()
            )]
        );
        assert!(defines(graph, &view).is_empty());
    }

    /// A file with only imports declares nothing: its `File` node stands,
    /// with no `Defines`.
    #[test]
    fn test_replay_new_file_with_only_imports_has_file_node_and_no_defines() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let mut only_imports = input("src/reexport.ts", Vec::new());
        only_imports
            .imports
            .push(crate::analyzer::types::RawImport {
                source: "./lib".to_string(),
                imported_name: "helper".to_string(),
                alias: None,
                binding_kind: None,
            });
        let view = OverlayView::build(graph, &[only_imports]).unwrap();
        assert_eq!(file_nodes(&view).len(), 1);
        assert!(defines(graph, &view).is_empty());
    }

    /// The base `File` node of a re-parsed path sources the rebuilt edges,
    /// and its stale base `Defines` (to a deleted and a kept declaration)
    /// are masked: each declaration has exactly one parent.
    #[test]
    fn test_replay_dirty_file_defines_start_at_base_file_and_mask_base_ones() {
        let mut fx = GraphFixture::new();
        let file = fx.node(NodeKind::File, "src/dirty.rs", "dirty.rs");
        let keep = fx.func("src/dirty.rs", "keep_fn");
        let gone = fx.func("src/dirty.rs", "gone_fn");
        fx.edge(file, keep, RelType::Defines);
        fx.edge(file, gone, RelType::Defines);
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let files = [input(
            "src/dirty.rs",
            vec![func("keep_fn", (1, 2)), func("new_fn", (4, 5))],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();

        assert!(file_nodes(&view).is_empty(), "the base File node stays");
        assert_eq!(view.redirect(file), Some(file));
        assert!(view.masks_base_edge(file, RelType::Defines));
        assert!(!view.masks_base_edge(file, RelType::Imports));
        assert_eq!(
            defines(graph, &view),
            [scope(file, base), scope(file, base + 1)]
        );
    }

    /// An edit that empties the file leaves its `File` node with no
    /// `Defines`.
    #[test]
    fn test_replay_emptied_file_keeps_file_node_and_drops_every_defines() {
        let mut fx = GraphFixture::new();
        let file = fx.node(NodeKind::File, "src/lib.ts", "lib.ts");
        let helper = fx.func("src/lib.ts", "helper");
        fx.edge(file, helper, RelType::Defines);
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let view = OverlayView::build(graph, &[input("src/lib.ts", Vec::new())]).unwrap();
        assert_eq!(view.redirect(file), Some(file));
        assert!(defines(graph, &view).is_empty());
    }

    /// A removed file has no parse: none of its `Defines` survive, and the
    /// replay makes no `File` node for it.
    #[test]
    fn test_replay_removed_file_has_no_file_node_and_no_defines() {
        let mut fx = GraphFixture::new();
        let file = fx.node(NodeKind::File, "src/gone.rs", "gone.rs");
        let gone = fx.func("src/gone.rs", "gone_fn");
        fx.edge(file, gone, RelType::Defines);
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let view =
            OverlayView::build_with_removed(graph, &[], &["src/gone.rs".to_string()]).unwrap();
        assert!(view.virtual_nodes().is_empty());
        assert!(defines(graph, &view).is_empty());
    }

    /// `foo` moves from `a.rs` to `b.rs`, both dirty: only `b.rs` defines it.
    #[test]
    fn test_replay_declaration_moved_between_dirty_files_defined_by_new_file_only() {
        let mut fx = GraphFixture::new();
        let a = fx.node(NodeKind::File, "src/a.rs", "a.rs");
        let b = fx.node(NodeKind::File, "src/b.rs", "b.rs");
        let foo = fx.func("src/a.rs", "foo");
        fx.edge(a, foo, RelType::Defines);
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let files = [
            input("src/a.rs", Vec::new()),
            input("src/b.rs", vec![func("foo", (1, 2))]),
        ];
        let view = OverlayView::build(graph, &files).unwrap();
        assert_eq!(view.redirect(foo), None, "a.rs no longer declares it");
        assert_eq!(defines(graph, &view), [scope(b, view.base_len())]);
    }

    /// Two inputs of one new path make two `File` nodes, as two
    /// `LocalGraph`s do; both inputs' declarations hang off the last one.
    #[test]
    fn test_replay_two_inputs_of_one_new_path_define_from_last_file_node() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [
            input("src/two.vue", vec![func("first", (1, 2))]),
            input("src/two.vue", vec![func("second", (4, 5))]),
        ];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        let files = file_nodes(&view);
        assert_eq!(files.len(), 2);
        let last = files[1].0;
        assert_eq!(
            defines(graph, &view),
            [scope(last, base), scope(last, base + 1)]
        );
    }

    /// `namespace Acme` twice: the index tombstones the second block, so the
    /// file defines the first one twice, and a member of the second block
    /// hangs off the first.
    #[test]
    fn test_replay_repeated_namespace_defines_survivor_twice_and_its_members() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/Lib.cs",
            vec![
                sym(NodeKind::Namespace, None, "Acme", (1, 9)),
                sym(NodeKind::Class, Some("Acme"), "Lib", (3, 8)),
                sym(NodeKind::Namespace, None, "Acme", (11, 19)),
                sym(NodeKind::Class, Some("Acme"), "Extra", (13, 18)),
            ],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        let file = base + 4;
        assert_eq!(
            view.node(base + 2).unwrap().name,
            "",
            "the second block is a tombstone"
        );
        assert_eq!(
            defines(graph, &view),
            [
                scope(base, base + 1),
                scope(base, base + 3),
                scope(file, base),
                scope(file, base),
            ]
        );
    }

    /// A Rust nested `fn` is scoped under its callable: no `Defines`.
    #[test]
    fn test_replay_scoped_nested_fn_gets_no_defines() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/n.rs",
            vec![func("outer", (1, 5)), func("inner", (2, 3))],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        assert_eq!(view.lexical_parent(base + 1), Some(base));
        assert_eq!(defines(graph, &view), [scope(base + 2, base)]);
    }

    #[test]
    fn test_replay_whitespace_only_name_is_defined_like_any_other() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let view =
            OverlayView::build(graph, &[input("src/w.rs", vec![func(" ", (1, 2))])]).unwrap();
        let base = view.base_len();
        assert_eq!(defines(graph, &view), [scope(base + 1, base)]);
    }

    /// Each enum defines its own variants; an enum with no variants defines
    /// none, and both enums hang off the file.
    #[test]
    fn test_replay_enum_variants_contained_and_empty_enum_defines_none() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/e.rs",
            vec![
                sym(NodeKind::Enum, None, "Color", (1, 4)),
                sym(NodeKind::EnumVariant, Some("Color"), "Red", (2, 2)),
                sym(NodeKind::EnumVariant, Some("Color"), "Green", (3, 3)),
                sym(NodeKind::Enum, None, "Empty", (5, 5)),
            ],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        let file = base + 4;
        let variant = |target| (base, target, ENUM_VARIANT_DEFINES_REASON.to_string());
        assert_eq!(
            defines(graph, &view),
            [
                variant(base + 1),
                variant(base + 2),
                scope(file, base),
                scope(file, base + 3)
            ]
        );
    }

    /// Two enums share a variant name: each variant pairs with its own enum
    /// by owner.
    #[test]
    fn test_replay_enums_sharing_a_variant_name_pair_by_owner() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/s.rs",
            vec![
                sym(NodeKind::Enum, None, "Shape", (1, 3)),
                sym(NodeKind::EnumVariant, Some("Shape"), "None", (2, 2)),
                sym(NodeKind::Enum, None, "Fill", (4, 6)),
                sym(NodeKind::EnumVariant, Some("Fill"), "None", (5, 5)),
            ],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        let pairs: Vec<(u32, u32)> = defines(graph, &view)
            .into_iter()
            .filter(|(_, _, reason)| reason == ENUM_VARIANT_DEFINES_REASON)
            .map(|(s, t, _)| (s, t))
            .collect();
        assert_eq!(pairs, [(base, base + 1), (base + 2, base + 3)]);
    }

    /// A dirty function's meta is the one its fragment paired, slot for
    /// slot; a function without one, and a base node, have none.
    #[test]
    fn test_replay_function_meta_carries_paired_flags_and_return_type() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let mut run = func("run", (1, 3));
        run.flags = Some(FunctionMeta::FLAG_ASYNC | FunctionMeta::FLAG_STATIC);
        run.return_type = Some("Promise<number>".to_string());
        let mut public = func("public_instance", (5, 6));
        public.flags = Some(0);
        let files = [input("src/f.ts", vec![run, func("plain", (8, 9)), public])];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();

        assert_eq!(
            view.function_meta(base),
            Some(&ViewFunctionMeta {
                node: base,
                flags: FunctionMeta::FLAG_ASYNC | FunctionMeta::FLAG_STATIC,
                return_type: Some("Promise<number>".to_string()),
            })
        );
        assert_eq!(view.function_meta(base + 1), None, "no meta paired");
        assert_eq!(
            view.function_meta(base + 2).map(|m| m.flags),
            Some(0),
            "zero flags are a value, not absence"
        );
        assert_eq!(view.function_meta(0), None, "a base node");
        assert_eq!(view.function_meta(base + 3), None, "the File node");
    }

    /// Two refreshes of one session build equal views and equal cache
    /// bytes, and the bytes decode to the same view, the base-sourced
    /// `Defines` and the function meta included.
    #[test]
    fn test_replay_same_inputs_twice_build_equal_views_and_cache_bytes() {
        let mut fx = GraphFixture::new();
        let file = fx.node(NodeKind::File, "src/dirty.rs", "dirty.rs");
        fx.func("src/dirty.rs", "keep_fn");
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let mut keep = func("keep_fn", (1, 2));
        keep.flags = Some(FunctionMeta::FLAG_TEST);
        let files = [
            input("src/dirty.rs", vec![keep]),
            input(
                "src/new.rs",
                vec![
                    sym(NodeKind::Enum, None, "E", (1, 3)),
                    sym(NodeKind::EnumVariant, Some("E"), "A", (2, 2)),
                ],
            ),
        ];
        let first = OverlayView::build(graph, &files).unwrap();
        let second = OverlayView::build(graph, &files).unwrap();
        assert_eq!(first, second);
        let cached = first.to_cache_bytes();
        assert!(!cached.is_empty(), "every reason is listed");
        assert_eq!(cached, second.to_cache_bytes());
        let decoded = OverlayView::from_cache_bytes(&cached, graph.nodes.len()).unwrap();
        assert_eq!(decoded, first);
        assert_eq!(
            decoded.overlay_out(file).count(),
            1,
            "the base File node's rebuilt edge decodes"
        );
        assert!(decoded.masks_base_edge(file, RelType::Defines));
        assert!(decoded.function_meta(decoded.base_len()).is_some());
    }
}
