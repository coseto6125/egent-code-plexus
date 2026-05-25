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

/// Scalar literal kinds that qualify an object pair value as a valid
/// enum-imitation member. Function, call, identifier, and template
/// expressions with substitutions are excluded: they indicate a plain
/// options object, not a discriminated-union enum imitation.
pub(crate) const SCALAR_VALUE_KINDS: &[&str] = &["number", "string", "true", "false", "null"];

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

/// True iff the first positional argument of `call_node` is a JS/TS string
/// literal (`"foo"`, `'foo'`, or a template string with no interpolation).
///
/// Shared by the TypeScript and JavaScript parsers — both grammars expose
/// the `arguments` field on `call_expression` with the same node-kind names
/// (`string` / `template_string` / `template_substitution`). Used to skip
/// `import("./foo")` / `require("fs")` from BlindSpot emission per
/// Constraint 2 of the cross-lang spec: literal-arg dynamic loads resolve
/// statically via the Imports edge and are NOT blind.
///
/// Other languages have different grammar (PHP wraps args in an extra
/// `argument` node; Ruby symbols are a separate node kind) and provide their
/// own helper.
#[inline]
pub fn js_ts_first_arg_is_literal_string(call_node: &tree_sitter::Node) -> bool {
    let Some(args) = call_node.child_by_field_name("arguments") else {
        return false;
    };
    let Some(first) = args.named_child(0) else {
        return false;
    };
    match first.kind() {
        "string" => true,
        "template_string" => {
            let mut cursor = first.walk();
            let has_interp = first
                .children(&mut cursor)
                .any(|c| c.kind() == "template_substitution");
            !has_interp
        }
        _ => false,
    }
}

/// Push a `BlindSpot` record into the parser-local Vec from a (kind, hint)
/// table entry and a captured tree-sitter node.
///
/// Centralises the seven-line boilerplate that previously repeated at every
/// `BLIND_SPEC[N]` dispatch arm across 15 parsers. Per FU-001 dispatcher-skel,
/// extracted AFTER P1–P7 shipped so the abstraction has 31 concrete call sites
/// motivating it (vs. the spec-warned single-data-point premature generalization).
///
/// `is_test_file` is the per-file value of `is_test_path(path)`, hoisted by
/// the caller once per `parse_file` invocation.
#[inline]
pub fn push_blind_spot(
    out: &mut Vec<ecp_core::analyzer::types::BlindSpot>,
    spec: (&str, &str),
    node: &tree_sitter::Node,
    path: &std::path::Path,
    is_test_file: bool,
) {
    out.push(ecp_core::analyzer::types::BlindSpot {
        kind: spec.0.to_string(),
        file_path: path.to_path_buf(),
        span: node_span(node),
        hint: spec.1.to_string(),
        is_test: is_test_file,
    });
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

/// Collect C / C++ attribute syntax attached to a declaration node — the C23
/// standard `[[nodiscard]]` / `[[deprecated]]` form (`attribute_declaration`)
/// and the GNU `__attribute__((...))` form (`attribute_specifier`).
///
/// Both forms appear as direct children of `function_definition` / `declaration`
/// in tree-sitter-c and tree-sitter-cpp (verified via probe). Raw text is
/// preserved verbatim so `normalize_decorator` can route on the bracket shape.
pub fn collect_cpp_attributes(decl_node: tree_sitter::Node<'_>, source: &[u8]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cursor = decl_node.walk();
    for child in decl_node.children(&mut cursor) {
        match child.kind() {
            "attribute_declaration" | "attribute_specifier" => {
                if let Ok(text) = std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                {
                    out.push(text.trim().to_string());
                }
            }
            _ => {}
        }
    }
    out
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

/// Find the innermost `Function`/`Method`/`Constructor` `RawNode` whose span
/// contains the point `(row, col)`. Returns the node's index into `nodes` as
/// `u32`, or `None` if no enclosing fn (module-level call).
///
/// Used by tx-scope detectors (Go / Ruby / Dart …) to recover the enclosing
/// function from a call-site position. Picks the smallest area via [`span_area`]
/// so nested-fn scenarios (`fn outer() { fn inner() { db.transaction(...) }}`)
/// resolve to the innermost match consistently across languages — previously
/// each detector inlined a divergent formula (Go used `dr * 10_000 + …`, Dart
/// used `<< 16` shift, Ruby used first-match with no min-area selection).
///
/// Constructor is included so a tx-scope inside `def initialize` (Ruby) or a
/// Dart `factory`/generative constructor resolves correctly.
pub fn enclosing_fn_idx_by_span(nodes: &[RawNode], row: u32, col: u32) -> Option<u32> {
    nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            matches!(
                n.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Constructor
            ) && point_in_span(n.span, row, col)
        })
        .min_by_key(|(_, n)| span_area(n.span))
        .map(|(idx, _)| idx as u32)
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
///
/// Also stamps nested type declarations (`Class | Interface | Trait | Struct |
/// Enum | Annotation`) so that inner classes with the same name across
/// different outer classes resolve to distinct UIDs
/// (uid = kind + path + owner_class + name).
///
/// Also stamps `EnumVariant` nodes: each variant gets `owner_class` = the
/// tightest enclosing `Enum` name. This is the span-containment equivalent of
/// the Rust parser's explicit `owner_class` assignment; all other languages
/// (TS / Java / Kotlin / C# / Swift / Dart / C++) rely on this pass because
/// their parsers emit variants as free `RawNode`s without an enclosing-enum
/// back-reference.
pub fn stamp_owner_class_by_span(nodes: &mut [RawNode]) {
    // Combined container pool: classes + namespaces/modules. Tightest span wins
    // regardless of container kind so a method inside `class Foo` inside
    // `namespace App` gets `owner_class = "Foo"` (the class, not the namespace).
    // PR #359's `scope_defines::Pass2` then emits `Namespace → child` only for
    // nodes whose owner_class matches a Namespace/Module name in the same file
    // — class members fall through cleanly because their owner is a class.
    let owner_spans: Vec<(String, Span)> = nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Class
                    | NodeKind::Struct
                    | NodeKind::Trait
                    | NodeKind::Interface
                    | NodeKind::Namespace
                    | NodeKind::Module
            )
        })
        .map(|n| (n.name.clone(), n.span))
        .collect();

    // Separate enum spans — used to stamp EnumVariant owner_class.
    let enum_spans: Vec<(String, Span)> = nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Enum))
        .map(|n| (n.name.clone(), n.span))
        .collect();

    if owner_spans.is_empty() && enum_spans.is_empty() {
        return;
    }

    for node in nodes.iter_mut() {
        let span = node.span;
        let owner = if matches!(
            node.kind,
            NodeKind::Method | NodeKind::Function | NodeKind::Constructor | NodeKind::Property
        ) {
            // Members: find the tightest enclosing container span.
            owner_spans
                .iter()
                .filter(|(_, s)| span_contains(*s, span))
                .min_by_key(|(_, s)| span_area(*s))
                .map(|(name, _)| name.clone())
        } else if matches!(
            node.kind,
            NodeKind::Class
                | NodeKind::Interface
                | NodeKind::Trait
                | NodeKind::Struct
                | NodeKind::Enum
                | NodeKind::Annotation
                | NodeKind::Namespace
                | NodeKind::Module
        ) {
            // Nested type / nested namespace declarations: find the tightest
            // enclosing container span that is strictly larger (exclude
            // self-containment where spans are equal).
            owner_spans
                .iter()
                .filter(|(_, s)| *s != span && span_contains(*s, span))
                .min_by_key(|(_, s)| span_area(*s))
                .map(|(name, _)| name.clone())
        } else if matches!(node.kind, NodeKind::EnumVariant) {
            // Variant → tightest enclosing Enum span.
            enum_spans
                .iter()
                .filter(|(_, s)| span_contains(*s, span))
                .min_by_key(|(_, s)| span_area(*s))
                .map(|(name, _)| name.clone())
        } else {
            continue;
        };
        // Preserve owner_class set explicitly by the parser (e.g. Rust's
        // enclosing-impl detection at emit time). Span-based stamping only
        // fills in the gap when nothing else has claimed ownership.
        if owner.is_some() && node.owner_class.is_none() {
            node.owner_class = owner;
        }
    }
}

/// Strip Python string-literal delimiters from `raw` source text.
///
/// Returns `Some(inner)` when `raw` is a well-formed Python string literal;
/// `inner` is the content between the outer delimiters. Returns `None`
/// otherwise (non-string token, malformed unmatched quote, etc.).
///
/// Covers:
/// - All prefix permutations: `r`, `b`, `u`, `f`, `rb`/`br`, `rf`/`fr`,
///   and their uppercase/mixed-case forms (matches CPython lexer rules).
/// - Triple-quote forms `"""…"""` and `'''…'''`.
/// - Plain single/double forms `"…"` and `'…'`.
///
/// Case-insensitive prefix matching is intentional: CPython accepts `RB"x"`,
/// `Rb"x"`, `rB"x"` etc. as valid byte-string prefixes.
pub fn strip_python_string_quotes(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'r' | b'R' | b'b' | b'B' | b'u' | b'U' | b'f' | b'F' => i += 1,
            _ => break,
        }
    }
    let quote_char = *bytes.get(i)?;
    if quote_char != b'"' && quote_char != b'\'' {
        return None;
    }
    if bytes.get(i + 1) == Some(&quote_char) && bytes.get(i + 2) == Some(&quote_char) {
        let body_start = i + 3;
        let body_end = bytes.len().checked_sub(3)?;
        if body_end < body_start {
            return None;
        }
        if bytes[body_end] != quote_char
            || bytes[body_end + 1] != quote_char
            || bytes[body_end + 2] != quote_char
        {
            return None;
        }
        return std::str::from_utf8(&bytes[body_start..body_end]).ok();
    }
    let body_start = i + 1;
    let body_end = bytes.len().checked_sub(1)?;
    if body_end < body_start || bytes[body_end] != quote_char {
        return None;
    }
    std::str::from_utf8(&bytes[body_start..body_end]).ok()
}

/// Nested-definition owner stamping for Python / JavaScript / TypeScript.
///
/// After `stamp_owner_class_by_span` has set `owner_class` for class members,
/// some nodes remain with `owner_class = None` even though they are physically
/// nested inside another function (e.g. `def wrapper()` inside a decorator
/// outer function, `function list()` inside another JS function). Two such
/// nodes with the same name but different outer functions share the uid
/// `(kind, path, "", name)` → uid-collision BlindSpot.
///
/// This pass resolves that: for every node whose `owner_class` is still `None`
/// AND whose span is wholly contained inside a `Function`/`Method` node's span,
/// set `owner_class` to the **innermost** such enclosing function's name.
///
/// Top-level (module-scope) definitions are untouched — they have no enclosing
/// function, so their `owner_class` remains `None` (correct behaviour: module-
/// level names are unique by name within a file).
///
/// Complexity: O(N·F) where N = total nodes, F = function count per file.
/// Both are small in practice (typical files: <100 nodes, <30 functions).
pub fn stamp_owner_fn_by_span(nodes: &mut [RawNode]) {
    // Snapshot (name, span) for all Function/Method nodes. We collect by value
    // so the subsequent mutable iteration over `nodes` doesn't borrow-conflict.
    let fn_spans: Vec<(String, Span)> = nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .map(|n| (n.name.clone(), n.span))
        .collect();
    if fn_spans.is_empty() {
        return;
    }
    for node in nodes.iter_mut() {
        // Only stamp nodes that didn't receive an owner from the class pass.
        if node.owner_class.is_some() {
            continue;
        }
        let span = node.span;
        // Find the innermost enclosing Function/Method.
        let owner = fn_spans
            .iter()
            // The enclosing function must *strictly* contain this node —
            // a function cannot be its own owner.
            .filter(|(_, s)| *s != span && span_contains(*s, span))
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

/// Match a .NET `[Transactional]` / `[TransactionAttribute]` C# attribute_list
/// string as captured by tree-sitter (the entire `[...]` text).
#[inline]
pub fn is_dotnet_transactional(decorator: &str) -> bool {
    decorator == "[Transactional]"
        || decorator == "[TransactionAttribute]"
        || decorator.starts_with("[Transactional(")
        || decorator.starts_with("[TransactionAttribute(")
}

/// Collect `RawTxScope` entries for Method / Constructor / Function nodes whose
/// decorator list contains a .NET `[Transactional]` attribute.
pub fn collect_dotnet_transactional_scopes(
    nodes: &[RawNode],
    scopeable_kinds: &[NodeKind],
) -> Option<Box<[RawTxScope]>> {
    let scopes: Vec<RawTxScope> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            scopeable_kinds.contains(&n.kind)
                && n.decorators.iter().any(|d| is_dotnet_transactional(d))
        })
        .map(|(idx, _)| RawTxScope::new(idx as u32, FrameworkId::DotNetTransactional))
        .collect();
    (!scopes.is_empty()).then(|| scopes.into_boxed_slice())
}

/// Match a Symfony PHP 8+ `#[Transactional]` attribute_list string as
/// captured by tree-sitter (the entire `#[...]` text).
#[inline]
pub fn is_symfony_transactional(decorator: &str) -> bool {
    decorator == "#[Transactional]" || decorator.starts_with("#[Transactional(")
}

/// Collect `RawTxScope` entries for Method / Function nodes whose decorator
/// list contains a Symfony `#[Transactional]` attribute.
pub fn collect_symfony_transactional_scopes(
    nodes: &[RawNode],
    scopeable_kinds: &[NodeKind],
) -> Option<Box<[RawTxScope]>> {
    let scopes: Vec<RawTxScope> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            scopeable_kinds.contains(&n.kind)
                && n.decorators.iter().any(|d| is_symfony_transactional(d))
        })
        .map(|(idx, _)| RawTxScope::new(idx as u32, FrameworkId::SymfonyTransactional))
        .collect();
    (!scopes.is_empty()).then(|| scopes.into_boxed_slice())
}

// ── FU-2026-05-23-009 cross-lang expansion: annotation-form helpers ──

/// Match a TypeScript / NestJS `@Transactional` decorator string as
/// captured by tree-sitter. Covers bare `@Transactional` plus
/// argument-bearing forms `@Transactional({ propagation: ... })`.
/// The typeorm-transactional npm package is the dominant runtime.
#[inline]
pub fn is_typeorm_transactional(decorator: &str) -> bool {
    decorator == "@Transactional" || decorator.starts_with("@Transactional(")
}

/// Collect `RawTxScope` entries for Method / Function nodes whose decorator
/// list contains a TypeORM-style `@Transactional` decorator.
pub fn collect_typeorm_transactional_scopes(
    nodes: &[RawNode],
    scopeable_kinds: &[NodeKind],
) -> Option<Box<[RawTxScope]>> {
    let scopes: Vec<RawTxScope> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            scopeable_kinds.contains(&n.kind)
                && n.decorators.iter().any(|d| is_typeorm_transactional(d))
        })
        .map(|(idx, _)| RawTxScope::new(idx as u32, FrameworkId::TypeOrmTransactional))
        .collect();
    (!scopes.is_empty()).then(|| scopes.into_boxed_slice())
}

/// Match a Rust `#[transaction]` proc-macro attribute string as captured
/// by tree-sitter (the entire `#[...]` text). Covers sqlx / diesel /
/// sea-orm flavours plus argument-bearing forms `#[transaction(rollback)]`.
#[inline]
pub fn is_rust_transactional(decorator: &str) -> bool {
    decorator == "#[transaction]" || decorator.starts_with("#[transaction(")
}

/// Collect `RawTxScope` entries for Function / Method nodes whose decorator
/// list contains a Rust `#[transaction]` proc-macro attribute.
pub fn collect_rust_transactional_scopes(
    nodes: &[RawNode],
    scopeable_kinds: &[NodeKind],
) -> Option<Box<[RawTxScope]>> {
    let scopes: Vec<RawTxScope> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            scopeable_kinds.contains(&n.kind)
                && n.decorators.iter().any(|d| is_rust_transactional(d))
        })
        .map(|(idx, _)| RawTxScope::new(idx as u32, FrameworkId::RustTransaction))
        .collect();
    (!scopes.is_empty()).then(|| scopes.into_boxed_slice())
}

// ── FU-2026-05-23-009 cross-lang expansion: NON-annotation patterns ──
//
// The four FrameworkId slots below (GoSqlTx, RubyActiveRecordTransaction,
// DartTransaction, SwiftTransactional) need detector patterns that DON'T
// dispatch off a decorator string — they're call-site / block-form. Each
// parser implements the detection inline (no centralised helper):
//
// **Go (FrameworkId::GoSqlTx)** — walk the AST for call expressions
// matching `db.Begin()` / `db.BeginTx(ctx, opts)` / receiver-typed
// `*sql.DB.Begin()` / `*gorm.DB.Begin()`. Recover the enclosing
// Function via `enclosing_function_name` + span containment. Emit ONE
// RawTxScope per enclosing function (not per call site — multiple
// `db.Begin()` in the same function = one scope).
//
// **Ruby (FrameworkId::RubyActiveRecordTransaction)** — walk for
// `method_call` nodes whose method is `transaction` followed by a
// `do_block` (ActiveRecord / Sequel idiom). Recover enclosing function;
// emit one scope per enclosing fn. This consolidates the block-form
// scope previously carved out as FU-2026-05-23-018.
//
// **Dart (FrameworkId::DartTransaction)** — Drift's
// `database.transaction(() async { ... })`. Detect the `transaction`
// method call where the argument is a closure / function expression.
// If no recognised framework, emit zero scopes (audit-only outcome OK).
//
// **Swift (FrameworkId::SwiftTransactional)** — Core Data
// `context.performAndWait { ... }` or GRDB `dbQueue.write { ... }`.
// Slot reserved; parser may emit zero scopes if no recognised framework
// is found — surface that as an audit finding in the PR rather than
// forcing a synthetic detector.

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

    fn fn_node(name: &str, span: Span, kind: NodeKind) -> RawNode {
        RawNode {
            name: name.to_string(),
            kind,
            span,
            is_exported: false,
            heritage: Vec::new(),
            type_annotation: None,
            decorators: Vec::new(),
            calls: Vec::new(),
            field_reads: Vec::new(),
            owner_class: None,
            content_hash: 0,
        }
    }

    #[test]
    fn enclosing_fn_idx_picks_innermost_when_nested() {
        // outer spans rows 0..20; inner spans rows 5..10. A point at (row=7, col=3)
        // falls inside both — helper must pick the smaller-area inner fn. The
        // pre-FU-034 Ruby detector used first-match `find_map` and would have
        // returned `outer` here in DFS-pre-order parser emission.
        let nodes = vec![
            fn_node("outer", (0, 0, 20, 0), NodeKind::Function),
            fn_node("inner", (5, 0, 10, 0), NodeKind::Function),
        ];
        let idx = enclosing_fn_idx_by_span(&nodes, 7, 3).expect("must find enclosing fn");
        assert_eq!(idx, 1, "innermost (inner) fn idx expected");
    }

    #[test]
    fn enclosing_fn_idx_returns_none_for_module_level() {
        let nodes = vec![fn_node("foo", (10, 0, 20, 0), NodeKind::Function)];
        // Point above any fn span — module-level call site.
        assert!(enclosing_fn_idx_by_span(&nodes, 2, 0).is_none());
    }

    #[test]
    fn enclosing_fn_idx_handles_constructor_kind() {
        // Constructor must be picked when it's the innermost containing fn —
        // Ruby `def initialize` / Dart factory / generative constructor.
        let nodes = vec![
            fn_node("outer", (0, 0, 20, 0), NodeKind::Function),
            fn_node("init", (5, 0, 10, 0), NodeKind::Constructor),
        ];
        let idx = enclosing_fn_idx_by_span(&nodes, 7, 0).expect("must find enclosing");
        assert_eq!(
            idx, 1,
            "Constructor at idx 1 should win over outer Function"
        );
    }

    #[test]
    fn enclosing_fn_idx_ignores_non_fn_kinds() {
        // A Class span tighter than the Function span — must not be selected
        // (only Function/Method/Constructor are valid tx-scope owners).
        let nodes = vec![
            fn_node("C", (0, 0, 30, 0), NodeKind::Class),
            fn_node("m", (5, 0, 25, 0), NodeKind::Method),
        ];
        let idx = enclosing_fn_idx_by_span(&nodes, 10, 0).expect("must find enclosing");
        assert_eq!(idx, 1, "Method (idx 1) chosen even when Class is smaller");
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
