//! Per-language `FunctionMeta` extraction helpers.
//!
//! Each submodule exposes a single free function:
//!
//! ```rust,ignore
//! pub fn extract(
//!     root: tree_sitter::Node<'_>,
//!     source: &[u8],
//!     nodes: &[RawNode],
//!     file_category: FileCategory,
//! ) -> Vec<RawFunctionMeta>
//! ```
//!
//! Called at the end of each language parser's `parse_file`, after `nodes` is
//! finalized. Returns one `RawFunctionMeta` per Function/Method/Constructor
//! node (keyed by span). The builder converts these to `FunctionMeta` by
//! interning strings into the `StringPool`.

use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, NodeKind};
use tree_sitter::Node;

pub mod c;
pub mod cpp;
pub mod csharp;
pub mod dart;
pub mod go;
pub mod java;
pub mod javascript;
pub mod kotlin;
pub mod php;
pub mod python;
pub mod ruby;
pub mod rust_lang;
pub mod swift;
pub mod typescript;

/// Span-keyed index entry: `(span, &RawNode)`.
pub type FnSpan<'a> = ((u32, u32, u32, u32), &'a RawNode);

/// Tree-sitter `Node` span as `(start_row, start_col, end_row, end_col)`.
pub(super) fn ts_span(n: &Node<'_>) -> (u32, u32, u32, u32) {
    let s = n.start_position();
    let e = n.end_position();
    (s.row as u32, s.column as u32, e.row as u32, e.column as u32)
}

/// Raw source bytes covered by `n`, as `&str`. Returns `""` on invalid UTF-8
/// so callers can do `.contains()` / `.starts_with()` without panicking on
/// the rare non-UTF-8 region.
pub(super) fn node_text<'a>(n: &Node<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(&source[n.start_byte()..n.end_byte()]).unwrap_or("")
}

/// Recursive descent: returns true iff any descendant (or `node` itself)
/// has `node.kind() == kind`. Used by cpp/php/ruby extractors to detect
/// `yield_expression`, `throw_expression`, etc. anywhere inside a fn body.
pub(crate) fn subtree_contains_kind(node: Node<'_>, kind: &str) -> bool {
    if node.kind() == kind {
        return true;
    }
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            if subtree_contains_kind(cursor.node(), kind) {
                return true;
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    false
}

/// Find the first direct child of `node` matching `kind`. Used by
/// kotlin/dart/swift extractors to pick out specific child grammar nodes
/// without re-implementing the cursor walk per language.
pub(super) fn find_child_kind<'a>(node: &Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut c = node.walk();
    if !c.goto_first_child() {
        return None;
    }
    loop {
        let child = c.node();
        if child.kind() == kind {
            return Some(child);
        }
        if !c.goto_next_sibling() {
            break;
        }
    }
    None
}

/// Sorted span index over Function/Method/Constructor nodes — enables
/// `binary_search_by_key` lookups in `walk_fn_nodes` instead of O(N) `find`.
pub(super) fn build_span_index(nodes: &[RawNode]) -> Vec<FnSpan<'_>> {
    let mut v: Vec<FnSpan<'_>> = nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Constructor
            )
        })
        .map(|n| (n.span, n))
        .collect();
    v.sort_by_key(|(s, _)| *s);
    v
}

/// Shared `extract()` template — filters callable nodes, walks the tree,
/// and invokes the per-language `extract_one`.
///
/// `fn_kinds` lists tree-sitter node-kind strings that *might* carry a
/// callable; for each such node the walker checks the span against
/// `fn_spans` (binary search) before invoking `extract_one`.
pub(super) fn extract_with<F>(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    file_category: FileCategory,
    fn_kinds: &'static [&'static str],
    extract_one: F,
) -> Vec<RawFunctionMeta>
where
    F: Fn(&Node<'_>, &[u8], &RawNode, FileCategory) -> Option<RawFunctionMeta>,
{
    let fn_spans = build_span_index(nodes);
    if fn_spans.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<RawFunctionMeta> = Vec::with_capacity(fn_spans.len());
    walk_fn_nodes(
        root,
        source,
        &fn_spans,
        file_category,
        fn_kinds,
        &extract_one,
        &mut out,
    );
    out
}

fn walk_fn_nodes<'a, F>(
    node: Node<'a>,
    source: &[u8],
    fn_spans: &[FnSpan<'a>],
    file_category: FileCategory,
    fn_kinds: &[&str],
    extract_one: &F,
    out: &mut Vec<RawFunctionMeta>,
) where
    F: Fn(&Node<'_>, &[u8], &RawNode, FileCategory) -> Option<RawFunctionMeta>,
{
    if fn_kinds.contains(&node.kind()) {
        let span = ts_span(&node);
        if let Ok(i) = fn_spans.binary_search_by_key(&span, |(s, _)| *s) {
            if let Some(meta) = extract_one(&node, source, fn_spans[i].1, file_category) {
                out.push(meta);
            }
        }
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            walk_fn_nodes(
                cursor.node(),
                source,
                fn_spans,
                file_category,
                fn_kinds,
                extract_one,
                out,
            );
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}
