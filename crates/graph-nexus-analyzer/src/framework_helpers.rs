//! Shared helpers for framework-aware parser captures.
//!
//! Three language parsers (python / rust / typescript) all need to:
//!   1. Convert tree-sitter `Node` start/end positions to our `(row, col, row, col)` span tuple.
//!   2. Test whether one span contains another.
//!   3. Find the innermost enclosing `Function` / `Method` `RawNode` that covers a given span.
//!
//! This module consolidates those helpers so each parser stays focused on its own
//! grammar quirks, not span arithmetic.

use graph_nexus_core::analyzer::types::{RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;

pub type Span = (u32, u32, u32, u32);

/// Sentinel `source_name` for framework refs registered at module level
/// (e.g. Actix `#[get]` attribute macros, top-level Express `app.get(...)`).
pub const MODULE_LEVEL_SOURCE: &str = "<module>";

/// Extract `(start_row, start_col, end_row, end_col)` span from a tree-sitter node.
/// Uses saturating conversion: rows/cols exceeding `u32::MAX` clamp to the cap
/// rather than silently truncating to a wrong line/col.
#[inline]
pub fn node_span(node: &tree_sitter::Node) -> Span {
    let s = node.start_position();
    let e = node.end_position();
    (
        crate::calls::safe_row(s.row),
        u32::try_from(s.column).unwrap_or(u32::MAX),
        crate::calls::safe_row(e.row),
        u32::try_from(e.column).unwrap_or(u32::MAX),
    )
}

/// True iff `outer` (row,col,row,col) fully contains `inner`.
#[inline]
pub fn span_contains(outer: Span, inner: Span) -> bool {
    let (or1, oc1, or2, oc2) = outer;
    let (ir1, ic1, ir2, ic2) = inner;
    let starts_after = (or1, oc1) <= (ir1, ic1);
    let ends_before = (ir2, ic2) <= (or2, oc2);
    starts_after && ends_before
}

/// Area proxy (row-major byte count approximation) for picking the smallest enclosing span.
#[inline]
pub fn span_area(s: Span) -> u64 {
    let (r1, c1, r2, c2) = s;
    let dr = r2.saturating_sub(r1) as u64;
    let dc = c2 as u64 + 10_000u64.saturating_sub(c1 as u64);
    dr * 10_000 + dc
}

/// Find the innermost `Function`/`Method` `RawNode` that contains `inner_span`.
/// Returns the node's `name` clone, or `None` if no enclosing fn (module-level).
pub fn enclosing_function_name(nodes: &[RawNode], inner_span: Span) -> Option<String> {
    nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .filter(|n| span_contains(n.span, inner_span))
        .min_by_key(|n| span_area(n.span))
        .map(|n| n.name.clone())
}

/// Find the innermost `Class` `RawNode` containing `inner_span`.
/// Returns `(class_name, class_span)`, or `None` if no enclosing class
/// (module-level fn/call).
pub fn enclosing_class(nodes: &[RawNode], inner_span: Span) -> Option<(String, Span)> {
    nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Class))
        .filter(|n| span_contains(n.span, inner_span))
        .min_by_key(|n| span_area(n.span))
        .map(|n| (n.name.clone(), n.span))
}

/// Enumerate `Function`/`Method` `RawNode` whose span lies inside `class_span`,
/// skipping dunder methods (`__init__`, `__repr__`, ...) and `exclude_name`
/// (the caller — prevents self-fan-out).
///
/// Python parser currently emits class-bound `def`s as `NodeKind::Function`, so
/// we accept both kinds to stay grammar-agnostic.
pub fn enumerate_class_methods(
    nodes: &[RawNode],
    class_span: Span,
    exclude_name: &str,
) -> Vec<String> {
    nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .filter(|n| span_contains(class_span, n.span))
        .filter(|n| !(n.name.starts_with("__") && n.name.ends_with("__")))
        .filter(|n| n.name != exclude_name)
        .map(|n| n.name.clone())
        .collect()
}

/// True iff the file's imports include at least one source matching the given
/// module patterns. Match is prefix-based: a required `"django"` matches imports
/// from `"django"`, `"django.urls"`, `"django.dispatch"`, etc. JS/TS scoped
/// packages use `/` as the separator (`@nestjs/common`), so prefix `"@nestjs"`
/// also matches.
///
/// Both `RawImport.source` and `RawImport.imported_name` are checked: Python's
/// plain `import fastapi` records `imported_name="fastapi"` with empty source,
/// so name-side matching is required for that idiom.
///
/// Used as a gate before emitting framework-specific `RawFrameworkRef` — we only
/// claim "this is a FastAPI route" when the file actually imports FastAPI.
/// Reflection / blind_spots are NOT gated (they're not framework-specific).
pub fn has_import_from(imports: &[RawImport], modules: &[&str]) -> bool {
    fn matches_module(value: &str, module: &str) -> bool {
        if value == module {
            return true;
        }
        // Submodule under required prefix. Separator depends on language:
        //   `.`  — Python (django.urls under django), Java (java.util.List
        //          under java), Kotlin (io.ktor.server.routing under io.ktor)
        //   `/`  — JS/TS scoped packages (@nestjs/common under @nestjs)
        //   `\\` — PHP namespaces (Illuminate\Support under Illuminate)
        // Zero-alloc byte compare avoids `format!()` per pair.
        let v = value.as_bytes();
        let m = module.as_bytes();
        v.len() > m.len()
            && v.starts_with(m)
            && (v[m.len()] == b'.' || v[m.len()] == b'/' || v[m.len()] == b'\\')
    }
    imports.iter().any(|imp| {
        modules
            .iter()
            .any(|m| matches_module(&imp.source, m) || matches_module(&imp.imported_name, m))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_gate_matches_exact_and_submodule() {
        let imps = vec![
            RawImport {
                source: "django.urls".into(),
                imported_name: "path".into(),
                alias: None,
            },
            RawImport {
                source: "os".into(),
                imported_name: "path".into(),
                alias: None,
            },
        ];
        assert!(has_import_from(&imps, &["django.urls"]));
        assert!(has_import_from(&imps, &["django"])); // prefix match
        assert!(!has_import_from(&imps, &["fastapi"]));
        assert!(!has_import_from(&imps, &["djangoz"])); // not a dot/slash prefix
    }

    #[test]
    fn import_gate_handles_scoped_packages() {
        let imps = vec![RawImport {
            source: "@nestjs/common".into(),
            imported_name: "Controller".into(),
            alias: None,
        }];
        assert!(has_import_from(&imps, &["@nestjs/common"]));
        assert!(has_import_from(&imps, &["@nestjs"])); // scoped prefix
    }

    #[test]
    fn import_gate_matches_bare_python_import() {
        // `import fastapi` → source is empty, imported_name is "fastapi".
        let imps = vec![RawImport {
            source: "".into(),
            imported_name: "fastapi".into(),
            alias: None,
        }];
        assert!(has_import_from(&imps, &["fastapi"]));
    }
}
