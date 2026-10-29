//! Per-file state: the borrowed tree and source, the record buffers that
//! today's `parse_file` bodies keep as locals, and the `LocalGraph` assembly.

use super::node;
use super::tap::TapRow;
use super::KindCapture;
use crate::calls::CallSiteIndex;
use crate::framework_helpers::push_blind_spot;
use ecp_core::analyzer::types::{
    BlindSpot, FrameworkId, LocalGraph, RawCallMeta, RawEventTopic, RawExportAlias, RawFanoutRef,
    RawFrameworkRef, RawFunctionMeta, RawImport, RawNode, RawPathLiteral, RawRoute,
    RawRouteReceiver, RawSchemaField, RawSqlRef, RawTxScope, Span,
};
use ecp_core::contracts::routing::ParsedLanguage;
use ecp_core::file_category::{determine_category, is_test_path, Language};
use ecp_core::graph::{FileCategory, NodeKind};
use rustc_hash::FxHashSet;
use std::path::Path;
use tree_sitter::{Node, Tree};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestRule {
    /// `determine_category` on the path; a test file is `FileCategory::Test`.
    Category,
    /// Python: `is_test_path`, and the category is only `Test` or `Source`.
    PythonPath,
}

impl TestRule {
    pub fn classify(self, path: &Path) -> (FileCategory, bool) {
        match self {
            TestRule::Category => {
                let category = determine_category(path.to_str().unwrap_or(""));
                (category, category == FileCategory::Test)
            }
            TestRule::PythonPath => match is_test_path(&path.to_string_lossy()) {
                true => (FileCategory::Test, true),
                false => (FileCategory::Source, false),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeLanguage {
    /// TS, JS and Python read it off the path, as `file_identities` does.
    FromPath,
    Fixed(Language),
}

impl ScopeLanguage {
    pub fn resolve(self, path: &Path) -> Language {
        match self {
            ScopeLanguage::FromPath => Language::from_path(&path.to_string_lossy()),
            ScopeLanguage::Fixed(language) => language,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Anonymous {
    pub capture: &'static str,
    pub call_kinds: &'static [&'static str],
    pub key: AnonKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnonKey {
    Span,
    /// Java: its tree-sitter node id is in `FileCx::anon_ids`.
    NodeId,
    /// Python: some node already in `FileCx::nodes` has its span.
    AnyNodeSpan,
}

/// The declaration a match builds. The driver writes it only for a kind
/// capture; a role may write any field. Every write replaces the last one.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeclSlot<'t> {
    pub name: Option<Node<'t>>,
    pub root: Option<Node<'t>>,
    pub kind: Option<NodeKind>,
}

impl<'t> DeclSlot<'t> {
    pub fn write_kind(&mut self, rule: KindCapture, node: Node<'t>, kind: NodeKind) {
        match rule {
            KindCapture::NameOverwrite => {
                self.name = Some(node);
                self.kind = Some(kind);
            }
            KindCapture::NameKeepFirst => {
                self.name = Some(node);
                if self.kind.is_none() {
                    self.kind = Some(kind);
                }
            }
            KindCapture::RootOverwrite => {
                self.root = Some(node);
                self.kind = Some(kind);
            }
        }
    }
}

/// One file's parse state. The fields are today's `parse_file` locals with
/// the same types; a step writes them, `into_graph` moves them out.
pub struct FileCx<'t> {
    pub tree: &'t Tree,
    /// The original bytes, even when `PREPARE_SOURCE` patched what the tree
    /// parsed: every capture text and hash reads these.
    pub source: &'t [u8],
    pub path: &'t Path,
    pub category: FileCategory,
    pub is_test: bool,
    pub nodes: Vec<RawNode>,
    pub imports: Vec<RawImport>,
    pub blind_spots: Vec<BlindSpot>,
    pub routes: Vec<RawRoute>,
    pub route_receivers: Vec<RawRouteReceiver>,
    pub framework_refs: Vec<RawFrameworkRef>,
    pub fanout_refs: Vec<RawFanoutRef>,
    pub path_literals: Vec<RawPathLiteral>,
    pub sql_refs: Vec<RawSqlRef>,
    pub call_sites: CallSiteIndex,
    pub call_metas: Vec<RawCallMeta>,
    pub raw_function_metas: Vec<RawFunctionMeta>,
    /// Python's pending tx sites `(row, column, framework)`, bound by
    /// `TxRule::Sites`.
    pub tx_sites: Vec<(u32, u32, FrameworkId)>,
    pub tx_scopes: Option<Box<[RawTxScope]>>,
    pub event_topics: Vec<RawEventTopic>,
    pub schema_fields: Vec<RawSchemaField>,
    pub export_aliases: Option<Box<[RawExportAlias]>>,
    /// Shared with JS declarations, which insert their spans here too.
    pub anon_spans: FxHashSet<Span>,
    pub anon_ids: FxHashSet<usize>,
    /// Candidate schema records from the query pass; `Op::SchemaFields`
    /// drains them.
    pub(super) tap_rows: Vec<TapRow>,
}

impl<'t> FileCx<'t> {
    pub fn new(tree: &'t Tree, source: &'t [u8], path: &'t Path, rule: TestRule) -> Self {
        let (category, is_test) = rule.classify(path);
        Self {
            tree,
            source,
            path,
            category,
            is_test,
            nodes: Vec::new(),
            imports: Vec::new(),
            blind_spots: Vec::new(),
            routes: Vec::new(),
            route_receivers: Vec::new(),
            framework_refs: Vec::new(),
            fanout_refs: Vec::new(),
            path_literals: Vec::new(),
            sql_refs: Vec::new(),
            call_sites: CallSiteIndex::default(),
            call_metas: Vec::new(),
            raw_function_metas: Vec::new(),
            tx_sites: Vec::new(),
            tx_scopes: None,
            event_topics: Vec::new(),
            schema_fields: Vec::new(),
            export_aliases: None,
            anon_spans: FxHashSet::default(),
            anon_ids: FxHashSet::default(),
            tap_rows: Vec::new(),
        }
    }

    pub fn push_blind(&mut self, spec: (&str, &str), node: &Node<'_>) {
        push_blind_spot(&mut self.blind_spots, spec, node, self.path, self.is_test);
    }

    pub fn emit_anonymous(&mut self, node: Node<'_>, a: &Anonymous) {
        if !node::subtree_has_kind(node, a.call_kinds) {
            return;
        }
        let span = node::span(node);
        let fresh = match a.key {
            AnonKey::Span => self.anon_spans.insert(span),
            AnonKey::NodeId => self.anon_ids.insert(node.id()),
            AnonKey::AnyNodeSpan => !self.nodes.iter().any(|n| n.span == span),
        };
        if fresh {
            self.nodes.push(node::anonymous(node, self.source));
        }
    }

    /// Every `Option<Box<[_]>>` field built from a buffer is `None` when the
    /// buffer is empty.
    pub fn into_graph(self) -> LocalGraph {
        LocalGraph {
            file_path: self.path.to_path_buf(),
            content_hash: [0; 8],
            nodes: self.nodes,
            documents: Vec::new(),
            imports: self.imports,
            routes: self.routes,
            route_receivers: boxed(self.route_receivers),
            framework_refs: self.framework_refs,
            fanout_refs: self.fanout_refs,
            blind_spots: self.blind_spots,
            schema_fields: boxed(self.schema_fields),
            event_topics: boxed(self.event_topics),
            tx_scopes: self.tx_scopes,
            path_literals: boxed(self.path_literals),
            sql_refs: boxed(self.sql_refs),
            call_metas: self.call_metas,
            raw_function_metas: self.raw_function_metas,
            export_aliases: self.export_aliases,
            parsed_language: ParsedLanguage::ABSENT,
        }
    }
}

fn boxed<T>(v: Vec<T>) -> Option<Box<[T]>> {
    (!v.is_empty()).then(|| v.into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::node::{kind_nodes, rust_tree};

    fn write_twice(tree: &Tree, rule: KindCapture) -> (DeclSlot<'_>, Node<'_>) {
        let (a, b) = (
            tree.root_node().child(0).unwrap(),
            tree.root_node().child(1).unwrap(),
        );
        let mut d = DeclSlot::default();
        d.write_kind(rule, a, NodeKind::Method);
        d.write_kind(rule, b, NodeKind::Function);
        (d, b)
    }

    #[test]
    fn test_decl_slot_name_keep_first_keeps_first_kind() {
        let tree = rust_tree(b"fn a() {}\nfn b() {}\n");
        let (d, b) = write_twice(&tree, KindCapture::NameKeepFirst);
        assert_eq!(
            (d.name, d.kind, d.root),
            (Some(b), Some(NodeKind::Method), None)
        );
        let (d, b) = write_twice(&tree, KindCapture::NameOverwrite);
        assert_eq!(
            (d.name, d.kind, d.root),
            (Some(b), Some(NodeKind::Function), None)
        );
    }

    #[test]
    fn test_decl_slot_root_overwrite_sets_root_not_name() {
        let tree = rust_tree(b"fn a() {}\nfn b() {}\n");
        let (d, b) = write_twice(&tree, KindCapture::RootOverwrite);
        assert_eq!(
            (d.name, d.kind, d.root),
            (None, Some(NodeKind::Function), Some(b))
        );
    }

    #[test]
    fn test_emit_anonymous_each_key_dedupes_and_gates_on_call() {
        let src = b"fn f() { g(|| h()); g(|| 1); }";
        let tree = rust_tree(src);
        let mut closures = Vec::new();
        kind_nodes(tree.root_node(), "closure_expression", &mut closures);
        for key in [AnonKey::Span, AnonKey::NodeId, AnonKey::AnyNodeSpan] {
            let mut cx = FileCx::new(&tree, src, Path::new("a.rs"), TestRule::Category);
            let a = Anonymous {
                capture: "c",
                call_kinds: &["call_expression"],
                key,
            };
            for closure in [closures[0], closures[0], closures[1]] {
                cx.emit_anonymous(closure, &a);
            }
            let spans: Vec<Span> = cx.nodes.iter().map(|n| n.span).collect();
            assert_eq!(spans, [node::span(closures[0])], "{key:?}");
        }
    }
}
