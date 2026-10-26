//! The `Decorates` edges of one decorated declaration: which decorator
//! yields which edge, its target and confidence, and the shared
//! `Annotation` node a decorator nothing declares reaches.
//!
//! The index's post-process pass (`ecp-analyzer`
//! `post_process::decorates_edges`) and the overlay's replay for a dirty
//! file (`session::view`) call [`decorates_edges`] once per declaration.
//! Each caller resolves a decorator name with its own resolver (the index's
//! `Resolver::resolve_symbol` for a type) and numbers the `Annotation`
//! nodes in its own index space, so a replay that answers both for a dirty
//! file gets the edges a reindex gives.
//!
//! Only the kinds [`carries_decorators`] admits source an edge. A parser
//! marker ([`RawNode::is_parser_marker`]) is no decorator. Every other raw
//! decorator, in order, becomes `(lookup name, full name)` pairs by
//! [`normalize_decorator`]; each pair, in order:
//!
//! | The lookup name | Target | Confidence |
//! |---|---|---|
//! | resolves as a type | each declaration it resolves to | the resolver's |
//! | resolves to nothing | the `Annotation` node of the full name, one per full name in the graph ([`annotation_uid`]) | [`ANNOTATION_CONFIDENCE`] |
//!
//! A (source, target) pair takes one edge: a repeated decorator adds none,
//! and neither does a tombstoned twin whose slot merges into its survivor.

use crate::analyzer::types::RawNode;
use crate::graph::NodeKind;
use rustc_hash::FxHashSet;

/// The reason of every `Decorates` edge.
pub const DECORATES_REASON: &str = "post_process:decorates";

/// The confidence of a `Decorates` edge into an `Annotation` node: below a
/// resolved declaration's, high enough to traverse.
pub const ANNOTATION_CONFIDENCE: f32 = 0.8;

/// Whether a declaration of `kind` sources `Decorates` edges.
pub fn carries_decorators(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Function
            | NodeKind::Method
            | NodeKind::Constructor
            | NodeKind::Property
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Interface
            | NodeKind::Trait
    )
}

/// The uid of the `Annotation` node of `full_name`. The node has no owning
/// file, so its uid path is `<annotation>/<full_name>`: one node per full
/// name, stable across builds.
pub fn annotation_uid(full_name: &str) -> u64 {
    crate::uid::compute(
        NodeKind::Annotation,
        &format!("<annotation>/{full_name}"),
        None,
        full_name,
    )
}

/// The `Decorates` edges of the declaration `source` of `kind` with the raw
/// `decorators` (`RawNode.decorators`, parser markers included), each
/// handed to `emit` as (target, confidence) in the table's order.
/// `resolve` answers a lookup name as a type; `annotation` gives the node
/// of a full name no declaration answers, made on its first call. `seen`
/// holds the (source, target) pairs already emitted: one set per build.
pub fn decorates_edges(
    source: u32,
    kind: NodeKind,
    decorators: &[String],
    seen: &mut FxHashSet<(u32, u32)>,
    mut resolve: impl FnMut(&str) -> Vec<(u32, f32)>,
    mut annotation: impl FnMut(&str) -> u32,
    mut emit: impl FnMut(u32, f32),
) {
    if decorators.is_empty() || !carries_decorators(kind) {
        return;
    }
    for raw in decorators {
        // The Overrides pass reads the override sentinel, identity the C#
        // arity marker, the partial-type merge the C# partial marker.
        if RawNode::is_parser_marker(raw) {
            continue;
        }
        for (lookup, full_name) in normalize_decorator(raw) {
            if lookup.is_empty() {
                continue;
            }
            let resolved = resolve(&lookup);
            if resolved.is_empty() {
                let target = annotation(&full_name);
                if seen.insert((source, target)) {
                    emit(target, ANNOTATION_CONFIDENCE);
                }
                continue;
            }
            for (target, confidence) in resolved {
                if seen.insert((source, target)) {
                    emit(target, confidence);
                }
            }
        }
    }
}

/// Normalize a raw decorator/attribute/annotation string captured from any
/// language to a `(lookup_name, full_name)` pair list.
///
/// Returns `Vec` to handle multi-arg derive: `#[derive(Serialize, Deserialize)]`
/// yields `[("Serialize","Serialize"), ("Deserialize","Deserialize")]`.
///
/// Language-specific raw formats handled:
/// - Python: `property`, `functools.cached_property`, `app.get` (no prefix)
/// - TS/JS/Java/Kotlin/Swift/Dart: `@Foo`, `@Foo(args)` (leading `@`)
/// - C#: `[Foo]`, `[FooAttribute]` (bracket-wrapped; `Attribute` suffix stripped)
/// - PHP: `#[Foo]`, `#[Foo(args)]` (leading `#[`)
/// - Rust: `#[test]`, `#[derive(A, B)]` (leading `#[`; derive expanded)
///
/// Resolution semantics:
/// - `lookup_name` is the bare identifier used for `Resolver::resolve_symbol`.
/// - `full_name` is the canonical name stored on synthetic `Annotation` nodes
///   (dotted-module prefix kept: `functools.cached_property`).
/// - Parameterized forms drop arguments: `@Cached(ttl=60)` → `("Cached","Cached")`.
/// - Dotted last-segment for lookup: `@functools.cached_property` →
///   lookup `"cached_property"`, full `"functools.cached_property"`.
pub fn normalize_decorator(raw: &str) -> Vec<(String, String)> {
    let s = raw.trim();

    // ── Go: `//go:noinline`, `//go:linkname X Y`, `//go:embed pattern` ──
    // Lookup uses the bare directive name (noinline / linkname / embed);
    // full_name keeps the `go:` namespace so it does not collide with
    // user-space annotations of the same bare name. Arguments after the
    // directive (linkname target, embed glob) are dropped — the LLM signal
    // is "this symbol carries directive X", not the directive payload.
    if let Some(rest) = s.strip_prefix("//go:") {
        let directive = rest.split_whitespace().next().unwrap_or("");
        if directive.is_empty() {
            return vec![];
        }
        return vec![(directive.to_string(), format!("go:{}", directive))];
    }

    // ── Rust / PHP: `#[...]` ─────────────────────────────────────────────
    // `#[derive(A, B, C)]` → multiple pairs.  Other attrs → single pair.
    if let Some(inner) = s
        .strip_prefix("#[")
        .and_then(|t| t.strip_suffix(']'))
        .map(str::trim)
    {
        if let Some(args) = inner
            .strip_prefix("derive(")
            .and_then(|t| t.strip_suffix(')'))
        {
            // `derive(A, B, C)` → expand each argument.
            return args
                .split(',')
                .map(|a| {
                    let n = a.trim().to_string();
                    (n.clone(), n)
                })
                .filter(|(n, _)| !n.is_empty())
                .collect();
        }
        // Other Rust/PHP attrs: `#[test]`, `#[Route('/')]`.
        let name = bare_ident(inner);
        if name.is_empty() {
            return vec![];
        }
        return vec![(name.to_string(), name.to_string())];
    }

    // ── C / C++ standard attribute: `[[nodiscard]]`, `[[deprecated("msg")]]`,
    // `[[gnu::pure]]`. Strip the double brackets and pick the bare ident
    // (after any namespace prefix). Must come before the single-bracket
    // branch because `[[X]]` also matches `[X]` after one strip.
    if let Some(inner) = s
        .strip_prefix("[[")
        .and_then(|t| t.strip_suffix("]]"))
        .map(str::trim)
    {
        if inner.is_empty() {
            return vec![];
        }
        let raw = bare_ident(inner);
        // `gnu::pure` / `clang::nonnull` → keep last segment for lookup,
        // preserve the namespaced form as full_name so the Annotation node
        // distinguishes `gnu::pure` from a plain `pure`.
        let lookup = raw.rsplit("::").next().unwrap_or(raw);
        return vec![(lookup.to_string(), raw.to_string())];
    }

    // ── C / C++ GNU attribute: `__attribute__((nodiscard))` / `((pure))` ─
    // Take the first identifier inside the inner parens.
    if let Some(inner) = s
        .strip_prefix("__attribute__((")
        .and_then(|t| t.strip_suffix("))"))
        .map(str::trim)
    {
        if inner.is_empty() {
            return vec![];
        }
        let name = bare_ident(inner);
        if name.is_empty() {
            return vec![];
        }
        return vec![(name.to_string(), name.to_string())];
    }

    // ── C# / PHP attribute_list: `[Foo]` or `[Foo(args)]` ───────────────
    // tree-sitter-c-sharp `attribute_list` captures the full `[...]` text.
    if let Some(inner) = s.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
        // Skip if it looks like `#[...]` already handled above, or empty.
        let inner = inner.trim();
        if inner.is_empty() {
            return vec![];
        }
        let name = bare_ident(inner);
        // Strip C# `Attribute` suffix so `[AuthorizeAttribute]` → `Authorize`.
        let lookup = name.strip_suffix("Attribute").unwrap_or(name);
        return vec![(lookup.to_string(), name.to_string())];
    }

    // ── `@Foo` / `@Foo(args)` ────────────────────────────────────────────
    if let Some(rest) = s.strip_prefix('@') {
        let name = bare_dotted(rest);
        if name.is_empty() {
            return vec![];
        }
        let lookup = dotted_last(name);
        return vec![(lookup.to_string(), name.to_string())];
    }

    // ── Plain identifier / dotted path (Python) ──────────────────────────
    let name = bare_dotted(s);
    if name.is_empty() {
        return vec![];
    }
    let lookup = dotted_last(name);
    vec![(lookup.to_string(), name.to_string())]
}

/// Return the bare identifier portion of a raw decorator string, stopping at
/// `(`, `[`, space, or end.  E.g. `"Route('/path')"` → `"Route"`.
#[inline]
fn bare_ident(s: &str) -> &str {
    s.split(|c: char| c == '(' || c == '[' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .trim()
}

/// Like `bare_ident` but allows dots (dotted paths like `app.get`).
#[inline]
fn bare_dotted(s: &str) -> &str {
    s.split(|c: char| c == '(' || c == '[' || c == '{' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .trim_end_matches('.')
}

/// Last segment of a dotted name: `"functools.cached_property"` → `"cached_property"`.
#[inline]
fn dotted_last(s: &str) -> &str {
    s.rsplit('.').next().unwrap_or(s)
}

#[cfg(test)]
mod normalize_tests {
    use super::normalize_decorator;

    #[test]
    fn python_plain() {
        assert_eq!(
            normalize_decorator("property"),
            vec![("property".into(), "property".into())]
        );
    }

    #[test]
    fn python_dotted() {
        assert_eq!(
            normalize_decorator("functools.cached_property"),
            vec![("cached_property".into(), "functools.cached_property".into())]
        );
    }

    #[test]
    fn at_simple() {
        assert_eq!(
            normalize_decorator("@Override"),
            vec![("Override".into(), "Override".into())]
        );
    }

    #[test]
    fn at_parameterized() {
        assert_eq!(
            normalize_decorator("@Cached(ttl=60)"),
            vec![("Cached".into(), "Cached".into())]
        );
    }

    #[test]
    fn at_dotted() {
        assert_eq!(
            normalize_decorator("@functools.cached_property"),
            vec![("cached_property".into(), "functools.cached_property".into())]
        );
    }

    #[test]
    fn csharp_bracket() {
        assert_eq!(
            normalize_decorator("[Authorize]"),
            vec![("Authorize".into(), "Authorize".into())]
        );
    }

    #[test]
    fn csharp_attribute_suffix_stripped() {
        assert_eq!(
            normalize_decorator("[AuthorizeAttribute]"),
            vec![("Authorize".into(), "AuthorizeAttribute".into())]
        );
    }

    #[test]
    fn rust_test_attr() {
        assert_eq!(
            normalize_decorator("#[test]"),
            vec![("test".into(), "test".into())]
        );
    }

    #[test]
    fn rust_derive_single() {
        assert_eq!(
            normalize_decorator("#[derive(Serialize)]"),
            vec![("Serialize".into(), "Serialize".into())]
        );
    }

    #[test]
    fn rust_derive_multi() {
        assert_eq!(
            normalize_decorator("#[derive(Serialize, Deserialize)]"),
            vec![
                ("Serialize".into(), "Serialize".into()),
                ("Deserialize".into(), "Deserialize".into()),
            ]
        );
    }

    #[test]
    fn go_pragma_noinline() {
        assert_eq!(
            normalize_decorator("//go:noinline"),
            vec![("noinline".into(), "go:noinline".into())]
        );
    }

    #[test]
    fn go_pragma_linkname_with_args() {
        // Arguments after directive name are dropped — LLM signal is the
        // directive, not the linkname target.
        assert_eq!(
            normalize_decorator("//go:linkname localname pkg.Symbol"),
            vec![("linkname".into(), "go:linkname".into())]
        );
    }

    #[test]
    fn go_pragma_build_skipped_at_emit_site() {
        // `//go:build` IS accepted by normalize but the Go parser never emits
        // it (file-level constraint, not symbol decorator). This test pins the
        // normalize contract for any unexpected caller.
        assert_eq!(
            normalize_decorator("//go:build linux"),
            vec![("build".into(), "go:build".into())]
        );
    }

    #[test]
    fn cpp_attribute_nodiscard() {
        assert_eq!(
            normalize_decorator("[[nodiscard]]"),
            vec![("nodiscard".into(), "nodiscard".into())]
        );
    }

    #[test]
    fn cpp_attribute_deprecated_with_args() {
        assert_eq!(
            normalize_decorator("[[deprecated(\"use NewFn\")]]"),
            vec![("deprecated".into(), "deprecated".into())]
        );
    }

    #[test]
    fn cpp_attribute_namespaced() {
        // `[[gnu::pure]]` → lookup "pure" (last segment), full keeps "gnu::pure"
        // so synthetic Annotation nodes do not collide with a plain `pure`.
        assert_eq!(
            normalize_decorator("[[gnu::pure]]"),
            vec![("pure".into(), "gnu::pure".into())]
        );
    }

    #[test]
    fn c_gnu_attribute() {
        assert_eq!(
            normalize_decorator("__attribute__((deprecated))"),
            vec![("deprecated".into(), "deprecated".into())]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::types::OVERRIDE_SENTINEL;
    use std::cell::RefCell;

    /// The edges of one declaration as (target, confidence), with `known`
    /// answering the lookups and each new full name taking `100 + k`. The
    /// second value lists the full names `annotation` was asked for.
    fn edges_of(
        kind: NodeKind,
        decorators: &[&str],
        known: &[(&str, u32, f32)],
    ) -> (Vec<(u32, f32)>, Vec<String>) {
        let decorators: Vec<String> = decorators.iter().map(|d| d.to_string()).collect();
        let asked: RefCell<Vec<String>> = RefCell::default();
        let mut made: Vec<String> = Vec::new();
        let mut out = Vec::new();
        decorates_edges(
            7,
            kind,
            &decorators,
            &mut FxHashSet::default(),
            |name| {
                known
                    .iter()
                    .filter(|(n, _, _)| *n == name)
                    .map(|&(_, target, confidence)| (target, confidence))
                    .collect()
            },
            |full_name| {
                asked.borrow_mut().push(full_name.to_string());
                let at = made.iter().position(|m| m == full_name).unwrap_or_else(|| {
                    made.push(full_name.to_string());
                    made.len() - 1
                });
                100 + at as u32
            },
            |target, confidence| out.push((target, confidence)),
        );
        (out, asked.into_inner())
    }

    /// A decorator a declaration answers reaches it with the resolver's
    /// confidence; an unresolved one reaches its `Annotation` at 0.8.
    #[test]
    fn test_decorates_edges_resolved_and_unresolved_targets() {
        let (edges, asked) = edges_of(
            NodeKind::Class,
            &["@Injectable()", "@Foo"],
            &[("Injectable", 3, 0.95)],
        );
        assert_eq!(edges, [(3, 0.95), (100, ANNOTATION_CONFIDENCE)]);
        assert_eq!(asked, ["Foo"]);
    }

    /// The same decorator twice, with arguments and without, takes one edge.
    #[test]
    fn test_decorates_edges_repeated_decorator_emits_once() {
        let (edges, _) = edges_of(NodeKind::Method, &["@Foo(1)", "@Foo"], &[]);
        assert_eq!(edges, [(100, ANNOTATION_CONFIDENCE)]);
    }

    /// A qualified decorator looks up its last segment and names its
    /// `Annotation` by the whole path.
    #[test]
    fn test_decorates_edges_qualified_decorator_keeps_full_name() {
        let (edges, asked) = edges_of(NodeKind::Function, &["@a.b.Foo"], &[]);
        assert_eq!(edges, [(100, ANNOTATION_CONFIDENCE)]);
        assert_eq!(asked, ["a.b.Foo"]);
    }

    /// Parser markers are no decorators; a kind that carries none, an
    /// empty list and a whitespace-only entry emit nothing.
    #[test]
    fn test_decorates_edges_markers_and_other_kinds_emit_nothing() {
        let arity = RawNode::generic_arity_marker(1);
        let (edges, asked) = edges_of(NodeKind::Class, &[OVERRIDE_SENTINEL, arity.as_str()], &[]);
        assert!(edges.is_empty() && asked.is_empty());
        assert!(edges_of(NodeKind::Variable, &["@Foo"], &[]).0.is_empty());
        assert!(edges_of(NodeKind::Class, &[], &[]).0.is_empty());
        assert!(edges_of(NodeKind::Class, &["   "], &[]).0.is_empty());
    }

    /// `#[derive(A, B)]` takes one edge per derived name.
    #[test]
    fn test_decorates_edges_derive_expands_each_name() {
        let (edges, asked) = edges_of(
            NodeKind::Struct,
            &["#[derive(Serialize, Deserialize)]"],
            &[],
        );
        assert_eq!(
            edges,
            [(100, ANNOTATION_CONFIDENCE), (101, ANNOTATION_CONFIDENCE)]
        );
        assert_eq!(asked, ["Serialize", "Deserialize"]);
    }

    /// Two resolved declarations each take an edge.
    #[test]
    fn test_decorates_edges_two_resolved_targets_emit_both() {
        let (edges, _) = edges_of(
            NodeKind::Class,
            &["@Foo"],
            &[("Foo", 4, 0.9), ("Foo", 5, 0.9)],
        );
        assert_eq!(edges, [(4, 0.9), (5, 0.9)]);
    }

    #[test]
    fn test_annotation_uid_matches_index_formula() {
        assert_eq!(
            annotation_uid("functools.cached_property"),
            crate::uid::compute(
                NodeKind::Annotation,
                "<annotation>/functools.cached_property",
                None,
                "functools.cached_property",
            )
        );
    }
}
