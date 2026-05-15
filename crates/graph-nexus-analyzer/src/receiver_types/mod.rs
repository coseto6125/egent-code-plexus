//! Shared primitives for receiver-type binding across language parsers.
//!
//! Each language analyzer that wants to rewrite `obj.method()` →
//! `Type.method` for the resolver's Tier 2.5 qualifier-scoped lookup
//! needs the same two pieces of machinery:
//!
//! 1. A nested-scope variable→type map keyed by row span, with
//!    smallest-enclosing-scope lookup (so closures correctly inherit
//!    outer annotations) — see [`ScopeMap`].
//! 2. An "enclosing class by line" lookup over `RawNode`s, for binding
//!    `this` / `self` / `super` / `base` receivers — see
//!    [`enclosing_class_by_line`].
//!
//! Both used to be hand-rolled per language (Python ref + 5 batch
//! branches × 2-4 languages each = 11 copies of the same ~20 LOC).
//! This module consolidates them; per-language `receiver_types.rs`
//! ships only its grammar-specific binding logic on top.
//!
//! Note: ScopeMap uses row-only spans because tree-sitter span endpoints
//! and `RawNode.span` rows are stable across the parsers; per-column
//! containment is overkill for fn / class body scoping (their open and
//! close braces don't span the call lines that matter).

use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
use std::collections::HashMap;

/// Stack of nested function scopes (by row span) → `var → type` bindings.
/// Lookup picks the smallest containing scope that has the variable, so
/// closures correctly inherit outer-scope annotations and inner scopes
/// override outer ones on name collision.
#[derive(Debug, Default)]
pub struct ScopeMap {
    scopes: Vec<((u32, u32), HashMap<String, String>)>,
}

impl ScopeMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a function-scope's bindings. Empty maps are dropped to
    /// keep lookup short.
    pub fn push(&mut self, span: (u32, u32), bindings: HashMap<String, String>) {
        if !bindings.is_empty() {
            self.scopes.push((span, bindings));
        }
    }

    /// Look up `var` at row `line`. Returns the type bound by the
    /// smallest enclosing scope, or `None` if no scope binds the name.
    pub fn lookup(&self, line: u32, var: &str) -> Option<&str> {
        let mut best: Option<&str> = None;
        let mut best_width = u32::MAX;
        for ((start, end), map) in &self.scopes {
            if *start <= line && line <= *end {
                if let Some(t) = map.get(var) {
                    let w = end - start;
                    if w < best_width {
                        best_width = w;
                        best = Some(t.as_str());
                    }
                }
            }
        }
        best
    }

    /// True when no bindings have been pushed.
    pub fn is_empty(&self) -> bool {
        self.scopes.is_empty()
    }
}

/// Find the innermost `Class` (or `Interface` when `include_interface`)
/// `RawNode` whose row span contains `line`. Used by per-language
/// receiver binding for `this` / `self` / `super` / `base` receivers.
///
/// Picks smallest-area first via `(end_row - start_row)`. Tied spans
/// resolve to the first node encountered, matching the Python
/// reference's behavior.
///
/// For column-precise containment with full `Span` quadruples use
/// [`crate::framework_helpers::enclosing_class`] instead — this fn
/// trades column precision for callers that only have `line` from a
/// call-site row.
pub fn enclosing_class_by_line(
    nodes: &[RawNode],
    line: u32,
    include_interface: bool,
) -> Option<&RawNode> {
    nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::Class)
                || (include_interface && matches!(n.kind, NodeKind::Interface))
        })
        .filter(|n| n.span.0 <= line && line <= n.span.2)
        .min_by_key(|n| n.span.2.saturating_sub(n.span.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_class(name: &str, start: u32, end: u32, heritage: Vec<String>) -> RawNode {
        RawNode {
            decorators: vec![],
            is_exported: true,
            heritage,
            type_annotation: None,
            name: name.to_string(),
            kind: NodeKind::Class,
            span: (start, 0, end, 0),
            calls: vec![],
        }
    }

    fn raw_iface(name: &str, start: u32, end: u32) -> RawNode {
        RawNode {
            decorators: vec![],
            is_exported: true,
            heritage: vec![],
            type_annotation: None,
            name: name.to_string(),
            kind: NodeKind::Interface,
            span: (start, 0, end, 0),
            calls: vec![],
        }
    }

    #[test]
    fn scope_map_lookup_picks_smallest_enclosing() {
        let mut m = ScopeMap::new();
        let mut outer = HashMap::new();
        outer.insert("x".to_string(), "Outer".to_string());
        let mut inner = HashMap::new();
        inner.insert("x".to_string(), "Inner".to_string());
        m.push((0, 100), outer);
        m.push((10, 20), inner);
        assert_eq!(m.lookup(15, "x"), Some("Inner"));
        assert_eq!(m.lookup(50, "x"), Some("Outer"));
        assert_eq!(m.lookup(15, "y"), None);
    }

    #[test]
    fn scope_map_drops_empty_bindings() {
        let mut m = ScopeMap::new();
        m.push((0, 100), HashMap::new());
        assert!(m.is_empty());
        assert_eq!(m.lookup(50, "x"), None);
    }

    #[test]
    fn scope_map_outer_fallthrough_when_inner_lacks_var() {
        let mut m = ScopeMap::new();
        let mut outer = HashMap::new();
        outer.insert("foo".to_string(), "Bar".to_string());
        let inner: HashMap<String, String> = HashMap::new();
        m.push((0, 100), outer);
        // Inner scope is empty so it's dropped — outer still wins.
        m.push((10, 20), inner);
        assert_eq!(m.lookup(15, "foo"), Some("Bar"));
    }

    #[test]
    fn enclosing_class_picks_innermost() {
        let nodes = vec![
            raw_class("Outer", 0, 100, vec![]),
            raw_class("Inner", 10, 50, vec!["Outer".into()]),
        ];
        let hit = enclosing_class_by_line(&nodes, 20, false).unwrap();
        assert_eq!(hit.name, "Inner");
        assert_eq!(hit.heritage, vec!["Outer".to_string()]);
    }

    #[test]
    fn enclosing_class_ignores_interface_by_default() {
        let nodes = vec![raw_iface("Iface", 0, 100)];
        assert!(enclosing_class_by_line(&nodes, 50, false).is_none());
        assert_eq!(
            enclosing_class_by_line(&nodes, 50, true).unwrap().name,
            "Iface"
        );
    }

    #[test]
    fn enclosing_class_none_when_line_outside() {
        let nodes = vec![raw_class("A", 0, 10, vec![])];
        assert!(enclosing_class_by_line(&nodes, 50, false).is_none());
    }
}
