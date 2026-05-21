//! Shared helpers for framework-aware parser captures.
//!
//! Three language parsers (python / rust / typescript) all need to:
//!   1. Convert tree-sitter `Node` start/end positions to our `(row, col, row, col)` span tuple.
//!   2. Test whether one span contains another.
//!   3. Find the innermost enclosing `Function` / `Method` `RawNode` that covers a given span.
//!
//! This module consolidates those helpers so each parser stays focused on its own
//! grammar quirks, not span arithmetic.

use ecp_core::analyzer::types::{FrameworkId, RawFrameworkRef, RawImport, RawNode, RawTxScope};
use ecp_core::graph::NodeKind;

pub type Span = (u32, u32, u32, u32);

/// One framework's textual signature: a name, a confidence value, a reason
/// tag, and the substrings that prove the framework is in use.
///
/// Mirrors upstream's `AstFrameworkPatternConfig` from
/// `_source_code/gitnexus/src/core/ingestion/language-provider.ts`. The
/// patterns are matched case-insensitively as substrings against the whole
/// file source, per upstream `detectFrameworkFromAST` (`framework-detection.ts`).
pub struct FrameworkPatternSpec {
    pub framework: &'static str,
    pub reason: &'static str,
    pub confidence: f32,
    pub patterns: &'static [&'static str],
}

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

/// True iff the raw source bytes at `node`'s start_byte begin with `prefix`.
///
/// Used by parsers whose grammars don't expose a visibility-modifier AST node —
/// the keyword sits in the source-text span but not in the parsed tree (e.g.
/// Zig `pub fn`, Move `public struct`). Forward direction: keyword is the
/// first token of the captured node.
#[inline]
pub fn node_source_starts_with(source: &[u8], node: tree_sitter::Node, prefix: &[u8]) -> bool {
    let start = node.start_byte();
    source
        .get(start..start.saturating_add(prefix.len()))
        .is_some_and(|s| s == prefix)
}

/// True iff the source bytes immediately preceding `node`'s start_byte —
/// after trimming trailing whitespace — end with `suffix`.
///
/// For grammars that don't tokenize the visibility keyword at all (e.g.
/// tree-sitter-cairo v0.0.1 strips `pub`), the keyword's bytes live BEFORE
/// the captured declaration's start_byte. The trim short-circuits at the
/// first non-whitespace from the end, so the scan cost is O(trailing-ws),
/// not O(file-prefix). Window size is `suffix.len() + 4` (slack for one or
/// two whitespace chars).
#[inline]
pub fn source_before_node_ends_with(source: &[u8], node: tree_sitter::Node, suffix: &[u8]) -> bool {
    let start = node.start_byte();
    let window = start.saturating_sub(suffix.len() + 4);
    source
        .get(window..start)
        .is_some_and(|s| s.trim_ascii_end().ends_with(suffix))
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

/// Find the innermost class-like `RawNode` containing `inner_span`.
/// Returns `(class_name, class_span)`, or `None` if no enclosing class
/// (module-level fn/call). Accepts `Class | Struct | Trait | Interface`
/// since the parity-14-langs work split Rust `struct` into its own variant.
pub fn enclosing_class(nodes: &[RawNode], inner_span: Span) -> Option<(String, Span)> {
    nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Class | NodeKind::Struct | NodeKind::Trait | NodeKind::Interface
            )
        })
        .filter(|n| span_contains(n.span, inner_span))
        .min_by_key(|n| span_area(n.span))
        .map(|n| (n.name.clone(), n.span))
}

/// Span-containment owner_class stamping for 12 languages whose parser captures
/// class members at emit time without a parent-walk (Java / Kotlin / C# / Swift /
/// Dart / JavaScript / TypeScript / Python / PHP / Ruby / C / C++). Snapshots
/// class spans once, then matches each `Method`/`Function`/`Constructor`/
/// `Property` against the snapshot — O(N+K·C) where N=nodes, K=members,
/// C=classes, vs O(N²) when each parser called `enclosing_class` in a map.
/// Rust uses `enclosing_impl_type` instead (impl blocks split struct/fn spans);
/// Go uses recv_map (explicit receiver types).
pub fn stamp_owner_class_by_span(nodes: &mut [RawNode]) {
    let class_spans: Vec<(String, Span)> = nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Class | NodeKind::Struct | NodeKind::Trait | NodeKind::Interface
            )
        })
        .map(|n| (n.name.clone(), n.span))
        .collect();
    if class_spans.is_empty() {
        return;
    }
    for node in nodes.iter_mut() {
        if !matches!(
            node.kind,
            NodeKind::Method | NodeKind::Function | NodeKind::Constructor | NodeKind::Property
        ) {
            continue;
        }
        let span = node.span;
        let owner = class_spans
            .iter()
            .filter(|(_, s)| span_contains(*s, span))
            .min_by_key(|(_, s)| span_area(*s))
            .map(|(name, _)| name.clone());
        if owner.is_some() {
            node.owner_class = owner;
        }
    }
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

/// Scan a file's source for any framework whose signature patterns appear,
/// emit one `RawFrameworkRef` per detected framework.
///
/// Mirrors upstream `detectFrameworkFromAST` (`framework-detection.ts:539`):
/// case-insensitive substring match against the whole source. Each detected
/// framework yields one ref with `source_name = MODULE_LEVEL_SOURCE` (the
/// signal is file-level — patterns are scattered across decorators / class
/// headers / call sites that we don't bind to a single enclosing function),
/// `target_name = framework`, and `span` pointing at the first matching
/// pattern's location for downstream attribution.
///
/// Each framework spec is emitted at most once per file (dedupe by framework
/// name) to avoid an explosion when many patterns of the same framework
/// appear in the same file.
pub fn detect_ast_framework_patterns(
    source: &[u8],
    specs: &[FrameworkPatternSpec],
) -> Vec<RawFrameworkRef> {
    let Ok(text) = std::str::from_utf8(source) else {
        return Vec::new();
    };
    let lowered = text.to_ascii_lowercase();
    let bytes = lowered.as_bytes();
    let mut out = Vec::new();
    for spec in specs {
        for pat in spec.patterns {
            let needle = pat.to_ascii_lowercase();
            if let Some(byte_pos) = find_subsequence(bytes, needle.as_bytes()) {
                let span = byte_position_to_span(text, byte_pos, byte_pos + needle.len());
                out.push(RawFrameworkRef {
                    source_name: MODULE_LEVEL_SOURCE.to_string(),
                    target_name: spec.framework.to_string(),
                    confidence: spec.confidence,
                    reason: spec.reason.to_string(),
                    span,
                });
                break;
            }
        }
    }
    out
}

/// Match a Spring `@Transactional` JVM annotation against a decorator string
/// captured by tree-sitter (which preserves the leading `@`). Covers both the
/// bare marker (`@Transactional`) and the parameterized form
/// (`@Transactional(propagation = ...)`). Shared by Java and Kotlin parsers.
#[inline]
pub fn is_jvm_transactional(decorator: &str) -> bool {
    decorator == "@Transactional" || decorator.starts_with("@Transactional(")
}

/// Collect `RawTxScope` entries for nodes whose `kind` is in `scopeable_kinds`
/// **and** carry an `@Transactional` decorator (Spring family). Both Java and
/// Kotlin parsers call this — they differ only in which `NodeKind`s are
/// scopeable (Kotlin adds `Function` for module-level `fun`; Java has no such
/// thing).
///
/// **Spring vs JPA**: currently emits `FrameworkId::SpringTransactional` for
/// every match. `FrameworkId::JpaTransactional` is reserved for a future
/// import-source dispatch (`javax.transaction.Transactional` /
/// `jakarta.transaction.Transactional`) — not implemented here because the
/// caller does not yet pass `RawImport` context. Adding that argument when
/// the JPA emit path lands is the planned extension point.
pub fn collect_jvm_transactional_scopes(
    nodes: &[RawNode],
    scopeable_kinds: &[NodeKind],
) -> Option<Box<[RawTxScope]>> {
    let scopes: Vec<RawTxScope> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            scopeable_kinds.contains(&n.kind)
                && n.decorators.iter().any(|d| is_jvm_transactional(d))
        })
        .map(|(idx, _)| RawTxScope::new(idx as u32, FrameworkId::SpringTransactional))
        .collect();
    (!scopes.is_empty()).then(|| scopes.into_boxed_slice())
}

/// Inclusive containment test: `(row, col)` lies within `span`'s
/// `(start_row, start_col, end_row, end_col)` range. Used to recover a
/// `RawNode` index from a captured identifier's position when the capture
/// fires inside the parser's match loop before `nodes` is finalized.
///
/// Distinct from [`span_contains`] which takes `(outer: Span, inner: Span)`
/// — keep the names separate so the call site reads the semantics
/// unambiguously.
#[inline]
pub fn point_in_span(span: Span, row: u32, col: u32) -> bool {
    let (sr, sc, er, ec) = span;
    let after_start = (row > sr) || (row == sr && col >= sc);
    let before_end = (row < er) || (row == er && col <= ec);
    after_start && before_end
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn byte_position_to_span(text: &str, start: usize, end: usize) -> Span {
    let (start_row, start_col) = byte_to_row_col(text, start);
    let (end_row, end_col) = byte_to_row_col(text, end.min(text.len()));
    (
        crate::calls::safe_row(start_row),
        u32::try_from(start_col).unwrap_or(u32::MAX),
        crate::calls::safe_row(end_row),
        u32::try_from(end_col).unwrap_or(u32::MAX),
    )
}

fn byte_to_row_col(text: &str, byte_pos: usize) -> (usize, usize) {
    let mut row = 0usize;
    let mut col = 0usize;
    for (i, b) in text.as_bytes().iter().enumerate() {
        if i == byte_pos {
            return (row, col);
        }
        if *b == b'\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (row, col)
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
                binding_kind: None,
            },
            RawImport {
                source: "os".into(),
                imported_name: "path".into(),
                alias: None,
                binding_kind: None,
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
            binding_kind: None,
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
            binding_kind: None,
        }];
        assert!(has_import_from(&imps, &["fastapi"]));
    }
}
