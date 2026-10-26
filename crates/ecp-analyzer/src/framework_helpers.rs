//! Shared helpers for framework-aware parser captures.
//!
//! Three language parsers (python / rust / typescript) all need to:
//!   1. Convert tree-sitter `Node` start/end positions to our `(row, col, row, col)` span tuple.
//!   2. Test whether one span contains another.
//!   3. Find the innermost enclosing `Function` / `Method` `RawNode` that covers a given span.
//!
//! This module consolidates those helpers so each parser stays focused on its own
//! grammar quirks, not span arithmetic.

use ecp_core::analyzer::lexical::lexical_scope_kinds;
use ecp_core::analyzer::types::{
    BindingKind, FrameworkId, RawExportAlias, RawFrameworkRef, RawImport, RawNode, RawTxScope,
};
use ecp_core::file_category::Language;
use ecp_core::graph::NodeKind;

pub use ecp_core::analyzer::types::{innermost_enclosing, span_contains, Span};

/// Scalar literal kinds that qualify an object pair value as a valid
/// enum-imitation member. Function, call, identifier, and template
/// expressions with substitutions are excluded: they indicate a plain
/// options object, not a discriminated-union enum imitation.
pub(crate) const SCALAR_VALUE_KINDS: &[&str] = &["number", "string", "true", "false", "null"];

/// One framework's textual signature: a name, a confidence value, a reason
/// tag, and the substrings that prove the framework is in use.
///
/// Same concept as GitNexus's `AstFrameworkPatternConfig` (behavior reference
/// only). The patterns are matched case-insensitively as substrings against the
/// whole file source, behavior cross-checked against GitNexus `detectFrameworkFromAST`.
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

pub use ecp_core::analyzer::membership::span_area;

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
/// `Property`/`Const` against the snapshot — O(N+K·C) where N=nodes, K=members,
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
    // An enum owns the methods, constructors and fields its body declares
    // (Java, Kotlin, Dart, PHP, Swift), exactly as a class does; without it
    // they read as free functions, and a type nested in an enum took the
    // enum's enclosing class as owner. An annotation type (C# attribute
    // class, Java `@interface`, Kotlin `annotation class`) owns its members
    // too; a use-site annotation (Dart `@override`) spans no member.
    let owner_spans: Vec<(String, Span)> = nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Class
                    | NodeKind::Struct
                    | NodeKind::Trait
                    | NodeKind::Interface
                    | NodeKind::Enum
                    | NodeKind::Annotation
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
            NodeKind::Method
                | NodeKind::Function
                | NodeKind::Constructor
                | NodeKind::Property
                | NodeKind::Const
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
/// nested inside another function (a local `class Fake` in a test function,
/// a variable). Two such nodes with the same name but different outer
/// functions share the uid `(kind, path, "", name)` → uid-collision BlindSpot.
///
/// This pass resolves that: for every node whose `owner_class` is still `None`
/// AND whose span is wholly contained inside a `Function`/`Method` node's span,
/// set `owner_class` to the **innermost** such enclosing function's name.
///
/// A kind `language` scopes lexically (`lexical_scope_kinds`: a nested
/// `def` / `function`, a lambda or callback node) gets no owner here: its
/// lexical parent places it and its uid hashes the parent's anchor
/// (`ecp_core::analyzer::lexical::clear_scoped_owners`).
///
/// Top-level (module-scope) definitions are untouched — they have no enclosing
/// function, so their `owner_class` remains `None` (correct behaviour: module-
/// level names are unique by name within a file).
///
/// Complexity: O(N·F) where N = total nodes, F = function count per file.
/// Both are small in practice (typical files: <100 nodes, <30 functions).
pub fn stamp_owner_fn_by_span(language: Language, nodes: &mut [RawNode]) {
    let scoped = lexical_scope_kinds(language);
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
        if node.owner_class.is_some() || scoped.contains(&node.kind) {
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

/// Mark the top-level declarations a JS / TS module exports in the forms the
/// declaration queries do not see: a local `export { f }` clause,
/// `export default f`, and an exported `declare` declaration. A declaration
/// exported under another name (`export { g as f }`) is marked as well:
/// `is_exported` means "reachable from another module", which is what the
/// resolver's import tier reads. A re-export (`export … from`) names another
/// module's declarations and marks nothing here.
///
/// Returns the file's export names with the declaration each binds when one
/// of them differs from its declaration's name (`export { g as f }`,
/// `export default h`, `export default wrap(h)` with an empty local), else
/// `None`: see `LocalGraph::export_aliases`.
pub fn stamp_js_ts_exports(
    root: tree_sitter::Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
) -> Option<Box<[RawExportAlias]>> {
    let mut exported: Vec<(&str, Span)> = Vec::new();
    // A set: every top-level declaration is checked against it below, and a
    // barrel file can export hundreds of names.
    let mut locals: rustc_hash::FxHashSet<&str> = rustc_hash::FxHashSet::default();
    // `(export name, local name)` of every export of the file.
    let mut names: Vec<(&str, &str)> = Vec::new();
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        if statement.kind() != "export_statement"
            || statement.child_by_field_name("source").is_some()
        {
            continue;
        }
        let mut tokens = statement.walk();
        let is_default = statement
            .children(&mut tokens)
            .any(|child| child.kind() == "default");
        if let Some(declaration) = statement.child_by_field_name("declaration") {
            let first = exported.len();
            js_ts_declared_names(declaration, source, &mut exported);
            names.extend(
                exported[first..]
                    .iter()
                    .map(|&(name, _)| (if is_default { "default" } else { name }, name)),
            );
            continue;
        }
        if let Some(value) = statement.child_by_field_name("value") {
            match js_ts_identifier(value, source) {
                Some(local) => {
                    locals.insert(local);
                    names.push(("default", local));
                }
                // `export default wrap(h)`: an empty local records that the
                // default binds no declaration.
                None => names.push(("default", "")),
            }
        }
        js_ts_clause_exports(statement, source, |export, local| {
            locals.insert(local);
            names.extend(export.map(|export| (export, local)));
        });
    }
    let aliases = js_ts_export_aliases(names);
    if !locals.is_empty() {
        let mut declarations = Vec::new();
        let mut cursor = root.walk();
        for declaration in root.named_children(&mut cursor) {
            js_ts_declared_names(declaration, source, &mut declarations);
        }
        exported.extend(
            declarations
                .into_iter()
                .filter(|(name, _)| locals.contains(name)),
        );
    }
    if exported.is_empty() {
        return aliases;
    }
    let mut by_span: rustc_hash::FxHashMap<Span, Vec<&str>> = rustc_hash::FxHashMap::default();
    for (name, span) in exported {
        by_span.entry(span).or_default().push(name);
    }
    for node in nodes.iter_mut() {
        if by_span
            .get(&node.span)
            .is_some_and(|names| names.iter().any(|name| *name == node.name))
        {
            node.is_exported = true;
        }
    }
    aliases
}

/// `names` as export aliases when some export name differs from its local
/// name. Each pair is kept once.
fn js_ts_export_aliases(mut names: Vec<(&str, &str)>) -> Option<Box<[RawExportAlias]>> {
    if names.iter().all(|(export, local)| export == local) {
        return None;
    }
    names.sort_unstable();
    names.dedup();
    Some(
        names
            .into_iter()
            .map(|(exported, local)| RawExportAlias {
                exported: exported.into(),
                local: local.into(),
            })
            .collect(),
    )
}

/// Each `(export name, local name)` of the `export { }` clauses of the
/// export statement `statement`, in source order. The export name is
/// `None` when it is no name [`js_ts_export_name`] reads.
fn js_ts_clause_exports<'s>(
    statement: tree_sitter::Node<'_>,
    source: &'s [u8],
    mut visit: impl FnMut(Option<&'s str>, &'s str),
) {
    let mut clauses = statement.walk();
    for clause in statement
        .named_children(&mut clauses)
        .filter(|child| child.kind() == "export_clause")
    {
        let mut specifiers = clause.walk();
        for specifier in clause.named_children(&mut specifiers) {
            let Some(local) = specifier
                .child_by_field_name("name")
                .and_then(|name| js_ts_identifier(name, source))
            else {
                continue;
            };
            let export = match specifier.child_by_field_name("alias") {
                Some(alias) => js_ts_export_name(alias, source),
                None => Some(local),
            };
            visit(export, local);
        }
    }
}

/// The name an `export { x as name }` gives: an identifier, `default`, or a
/// string (`export { x as "a-b" }`).
fn js_ts_export_name<'s>(node: tree_sitter::Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    let node = match node.kind() {
        "identifier" | "default" => node,
        "string" => node.named_child(0)?,
        _ => return None,
    };
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).ok()
}

/// [`BindingKind::EsReexport`] for a named import the TS / JS import
/// queries capture from a re-export (`export { x } from './m'`), which the
/// parsers emit like `import { x } from './m'`. `source` is the captured
/// specifier fragment; its string sits in the statement.
pub fn js_ts_reexport_kind(source: tree_sitter::Node<'_>) -> Option<BindingKind> {
    let statement = source.parent()?.parent()?;
    (statement.kind() == "export_statement").then_some(BindingKind::EsReexport)
}

/// Mark the binding kinds of `imports` that the import queries cannot see.
/// A default import (`import d from './m'`) is [`BindingKind::EsDefault`]:
/// the queries emit it like `import { d } from './m'`, which binds a
/// different export. An import the file's own `export { x }` clause exports
/// under its local name is [`BindingKind::EsReexport`], or
/// [`BindingKind::EsDefaultReexport`] for a default import. The parsers mark
/// an `export { x } from` re-export as they emit it
/// ([`js_ts_reexport_kind`]), which this leaves as it is.
pub fn mark_js_ts_import_bindings(
    root: tree_sitter::Node<'_>,
    source: &[u8],
    imports: &mut [RawImport],
) {
    let mut defaults: Vec<(&str, &str)> = Vec::new();
    // `export { x as y }` exports `y`, not the import `x`.
    let mut exported: rustc_hash::FxHashSet<&str> = rustc_hash::FxHashSet::default();
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        match statement.kind() {
            "import_statement" => {
                let Some(specifier) = statement
                    .child_by_field_name("source")
                    .and_then(|string| string.named_child(0))
                    .and_then(|fragment| {
                        std::str::from_utf8(&source[fragment.start_byte()..fragment.end_byte()])
                            .ok()
                    })
                else {
                    continue;
                };
                let mut clauses = statement.walk();
                for clause in statement
                    .named_children(&mut clauses)
                    .filter(|child| child.kind() == "import_clause")
                {
                    let mut bindings = clause.walk();
                    defaults.extend(
                        clause
                            .named_children(&mut bindings)
                            .filter_map(|binding| js_ts_identifier(binding, source))
                            .map(|local| (local, specifier)),
                    );
                }
            }
            "export_statement" if statement.child_by_field_name("source").is_none() => {
                js_ts_clause_exports(statement, source, |export, local| {
                    if export == Some(local) {
                        exported.insert(local);
                    }
                });
            }
            _ => {}
        }
    }
    if defaults.is_empty() && exported.is_empty() {
        return;
    }
    for import in imports
        .iter_mut()
        .filter(|import| import.binding_kind.is_none())
    {
        let default = import.alias.is_none()
            && defaults.iter().any(|&(local, specifier)| {
                import.imported_name == local && import.source == specifier
            });
        let reexported =
            exported.contains(import.alias.as_deref().unwrap_or(&import.imported_name));
        import.binding_kind = match (default, reexported) {
            (true, true) => Some(BindingKind::EsDefaultReexport),
            (true, false) => Some(BindingKind::EsDefault),
            (false, true) => Some(BindingKind::EsReexport),
            (false, false) => None,
        };
    }
}

/// The text of `node` when it is a plain identifier.
fn js_ts_identifier<'s>(node: tree_sitter::Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    if node.kind() != "identifier" {
        return None;
    }
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).ok()
}

/// `(name, span)` of every binding `declaration` introduces, with the spans
/// the parsers give its nodes.
fn js_ts_declared_names<'s>(
    declaration: tree_sitter::Node<'_>,
    source: &'s [u8],
    out: &mut Vec<(&'s str, Span)>,
) {
    let mut cursor = declaration.walk();
    match declaration.kind() {
        "lexical_declaration" | "variable_declaration" => {
            let span = node_span(&declaration);
            for declarator in declaration.named_children(&mut cursor) {
                let Some(name) = declarator
                    .child_by_field_name("name")
                    .and_then(|name| js_ts_identifier(name, source))
                else {
                    continue;
                };
                // A Const / Variable spans its declaration, a function-valued
                // binding its declarator.
                out.push((name, span));
                out.push((name, node_span(&declarator)));
            }
        }
        "ambient_declaration" => {
            for inner in declaration.named_children(&mut cursor) {
                js_ts_declared_names(inner, source, out);
            }
        }
        _ => {
            if let Some(name) = declaration.child_by_field_name("name").and_then(|name| {
                std::str::from_utf8(&source[name.start_byte()..name.end_byte()]).ok()
            }) {
                out.push((name, node_span(&declaration)));
            }
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
        //   `::` — Rust paths (rdkafka::producer under rdkafka)
        // Zero-alloc byte compare avoids `format!()` per pair.
        let v = value.as_bytes();
        let m = module.as_bytes();
        v.len() > m.len()
            && v.starts_with(m)
            && matches!(&v[m.len()..], [b'.' | b'/' | b'\\', ..] | [b':', b':', ..])
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

    fn import(source: &str) -> RawImport {
        RawImport {
            source: source.into(),
            imported_name: "x".into(),
            alias: None,
            binding_kind: None,
        }
    }

    #[test]
    fn test_has_import_from_rust_submodule_path_matches_crate() {
        assert!(has_import_from(
            &[import("rdkafka::producer")],
            &["rdkafka"]
        ));
        assert!(has_import_from(&[import("axum::routing")], &["axum"]));
        assert!(!has_import_from(
            &[import("rdkafkax::producer")],
            &["rdkafka"]
        ));
        assert!(!has_import_from(
            &[import("rdkafka:producer")],
            &["rdkafka"]
        ));
        assert!(!has_import_from(&[import("rdkafka:")], &["rdkafka"]));
    }

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

    /// Stands for: the fn-name owner reaches the non-callables of a function
    /// body only (a local class, a variable), so they keep today's uid; a
    /// nested function or lambda of a language that scopes `Function` gets
    /// none (slice D). A language that scopes nothing keeps the old stamp.
    #[test]
    fn test_stamp_owner_fn_by_span_scoped_kind_skipped_local_class_stamped() {
        let nodes = || {
            vec![
                fn_node("outer", (0, 0, 20, 0), NodeKind::Function),
                fn_node("inner", (1, 4, 10, 0), NodeKind::Function),
                fn_node("deep", (2, 8, 4, 0), NodeKind::Function),
                fn_node("<anonymous:6:8>", (5, 8, 5, 40), NodeKind::Function),
                fn_node("Fake", (11, 4, 13, 0), NodeKind::Class),
                fn_node("x", (14, 4, 14, 20), NodeKind::Variable),
                fn_node("top", (21, 0, 22, 0), NodeKind::Function),
            ]
        };
        let owners = |nodes: &[RawNode]| -> Vec<Option<String>> {
            nodes.iter().map(|n| n.owner_class.clone()).collect()
        };
        let outer = || Some("outer".to_string());
        for language in [Language::Python, Language::JavaScript, Language::TypeScript] {
            let mut stamped = nodes();
            stamp_owner_fn_by_span(language, &mut stamped);
            assert_eq!(
                owners(&stamped),
                [None, None, None, None, outer(), outer(), None],
                "{language:?}"
            );
        }
        let mut stamped = nodes();
        stamp_owner_fn_by_span(Language::Go, &mut stamped);
        assert_eq!(
            owners(&stamped),
            [
                None,
                outer(),
                Some("inner".to_string()),
                Some("inner".to_string()),
                outer(),
                outer(),
                None
            ],
            "control: a language that scopes nothing"
        );
        stamp_owner_fn_by_span(Language::Python, &mut []);
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

pub use ecp_core::analyzer::decorates::normalize_decorator;

#[cfg(test)]
mod innermost_enclosing_tests {
    use super::{innermost_enclosing, span_area, span_contains, Span};

    /// The pairwise rule the sweep replaces: innermost strictly-different
    /// container, first minimum in index order.
    fn pairwise(spans: &[Span]) -> Vec<Option<usize>> {
        spans
            .iter()
            .map(|&s| {
                spans
                    .iter()
                    .enumerate()
                    .filter(|(_, &c)| c != s && span_contains(c, s))
                    .min_by_key(|(_, &c)| span_area(c))
                    .map(|(i, _)| i)
            })
            .collect()
    }

    /// Deterministic nested tree: every span either nests inside the previous
    /// open one or starts after it closed, like functions in one file.
    fn nested_tree(seed: u64, count: usize) -> Vec<Span> {
        let mut state = seed;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };
        let mut spans = Vec::with_capacity(count);
        let mut open: Vec<Span> = Vec::new();
        let mut line = 1u32;
        while spans.len() < count {
            let roll = next() % 4;
            if roll == 0 && !open.is_empty() {
                // Leaving a span: the next one starts after it closed.
                line = open.pop().unwrap().2 + 1;
                continue;
            }
            let outer_end = open.last().map(|s| s.2).unwrap_or(u32::MAX / 2);
            let start = line + next() % 3;
            if start + 2 >= outer_end {
                line = open.pop().unwrap().2 + 1;
                continue;
            }
            let end = (start + 1 + next() % 40).min(outer_end - 1);
            let span = (start, next() % 8, end, 1 + next() % 80);
            spans.push(span);
            open.push(span);
            line = start + 1;
        }
        spans
    }

    #[test]
    fn test_innermost_enclosing_nested_tree_matches_pairwise_rule() {
        for seed in 1..40u64 {
            let spans = nested_tree(seed, 120);
            assert_eq!(innermost_enclosing(&spans), pairwise(&spans), "seed {seed}");
        }
    }

    #[test]
    fn test_innermost_enclosing_identical_spans_lower_index_wins_and_never_self() {
        let spans: Vec<Span> = vec![
            (1, 0, 50, 1),
            (1, 0, 50, 1),
            (2, 0, 10, 1),
            (2, 0, 10, 1),
            (3, 0, 4, 1),
            (20, 0, 30, 1),
            (60, 0, 70, 1),
        ];
        let parents = innermost_enclosing(&spans);
        assert_eq!(parents, pairwise(&spans));
        assert_eq!(
            parents,
            vec![None, None, Some(0), Some(0), Some(2), Some(0), None]
        );
    }

    #[test]
    fn test_innermost_enclosing_empty_and_single() {
        assert!(innermost_enclosing(&[]).is_empty());
        assert_eq!(innermost_enclosing(&[(1, 0, 2, 0)]), vec![None]);
    }
}
