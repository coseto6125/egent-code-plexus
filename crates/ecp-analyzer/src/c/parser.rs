use super::receiver_types::{
    collect_receiver_methods, extract_c_calls_and_path_literals, CReceiverMap,
};
use super::spec::CSpec;
use crate::calls::{NameRefKind, NameRefSpec, VariantRefSpec};
use crate::driver::{
    self, node, Compiled, DeclSlot, Effect, FieldReads, FileCx, KindCapture, Op, ParseSpec, Step,
};
use crate::indirect_dispatch::{collect_c_cpp_fn_ptr_vars, detect_c_cpp_indirect};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{BindingKind, LocalGraph, RawFunctionMeta, RawImport, RawNode};
use ecp_core::graph::{FileCategory, NodeKind};
use std::path::Path;
use tree_sitter::{Node, Query, Tree};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_c::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
/// True if `name` is a C reserved keyword that tree-sitter-c sometimes
/// mis-captures as an identifier when error-recovering from preprocessor
/// macros. Legal C code never names a variable with any of these, so
/// rejecting them only suppresses parse-recovery noise.
fn is_c_reserved_keyword(name: &str) -> bool {
    matches!(
        name,
        "void"
            | "char"
            | "short"
            | "int"
            | "long"
            | "float"
            | "double"
            | "signed"
            | "unsigned"
            | "_Bool"
            | "_Complex"
            | "_Imaginary"
            | "const"
            | "volatile"
            | "restrict"
            | "_Atomic"
            | "register"
            | "static"
            | "extern"
            | "auto"
            | "_Thread_local"
            | "struct"
            | "union"
            | "enum"
            | "typedef"
            | "if"
            | "else"
            | "for"
            | "while"
            | "do"
            | "switch"
            | "case"
            | "default"
            | "break"
            | "continue"
            | "return"
            | "goto"
            | "sizeof"
            | "inline"
            | "_Static_assert"
            | "_Alignas"
            | "_Alignof"
    )
}

/// Returns true if `node` (a `function_definition`) has a `static` storage class specifier
/// among its direct children.
fn has_static_specifier(node: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    let result = node.children(&mut cursor).any(|child| {
        child.kind() == "storage_class_specifier"
            && source
                .get(child.start_byte()..child.end_byte())
                .and_then(|b| std::str::from_utf8(b).ok())
                == Some("static")
    });
    result
}

/// Extract a type-annotation string for a param/field/variable declaration by
/// slicing the source from the outer declaration's start byte to the start of
/// the identifier name. This preserves the original spelling — qualifiers
/// (`const`, `static`), storage class, pointer / array operators (`*`, `[]`),
/// and the type specifier itself.
///
/// Convention (documented per task D3):
/// - **Pointer spacing** is preserved as-written. `const char* s` yields
///   `"const char*"`; `int * p` yields `"int *"`. Source is source of truth.
/// - **Qualifier inclusion** is YES. `static const int N` yields
///   `"static const int"`. Downstream consumers can strip storage-class
///   words if they want a bare type; the analyzer surfaces the full
///   declaration prefix because it's the most information-preserving and
///   cheap to compute (one byte-range slice).
fn slice_type_before(
    decl: tree_sitter::Node<'_>,
    name: tree_sitter::Node<'_>,
    source: &[u8],
) -> Option<String> {
    let start = decl.start_byte();
    let end = name.start_byte();
    if end <= start {
        return None;
    }
    std::str::from_utf8(source.get(start..end)?)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Drill into nested declarators (pointer / array / parenthesized / function)
/// to find the innermost `type_identifier` — the actual alias name for a
/// `typedef`.
///
/// Shapes handled:
/// - `typedef int Counter;`           → direct `type_identifier` child
/// - `typedef char** StrArray;`       → nested `pointer_declarator`
/// - `typedef int IntArr[10];`        → `array_declarator`
/// - `typedef int (*FnPtr)(int);`     → `function_declarator >
///   parenthesized_declarator > pointer_declarator > type_identifier`
fn find_typedef_alias_name<'a>(node: Node<'a>) -> Option<Node<'a>> {
    if node.kind() == "type_identifier" {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if matches!(
            child.kind(),
            "pointer_declarator"
                | "array_declarator"
                | "function_declarator"
                | "parenthesized_declarator"
                | "type_identifier"
        ) {
            if let Some(found) = find_typedef_alias_name(child) {
                return Some(found);
            }
        }
    }
    None
}

/// Heuristic: detect `#define FOO_H` / `#define FOO_H 1` style include
/// guards so they don't drown out real bindings.
///
/// Conservative: matches when the name ends with `_H`, `_HPP`, `_GUARD`,
/// or `_INCLUDED` AND the body is absent or a bare `1`. Real constants
/// like `MAX_SIZE` (no suffix match) or `BUFFER_SIZE 4096` (body not `1`)
/// pass through.
fn is_include_guard(name: &str, body: Option<&str>) -> bool {
    let suffix_match = name.ends_with("_H")
        || name.ends_with("_HPP")
        || name.ends_with("_GUARD")
        || name.ends_with("_INCLUDED");
    if !suffix_match {
        return false;
    }
    matches!(body.map(str::trim), None | Some("") | Some("1"))
}

/// Find the storage_class_specifier child with text `"extern"`.
fn extern_specifier<'a>(decl: Node<'a>, source: &[u8]) -> Option<Node<'a>> {
    let mut cursor = decl.walk();
    let found = decl.children(&mut cursor).find(|child| {
        child.kind() == "storage_class_specifier"
            && source
                .get(child.start_byte()..child.end_byte())
                .and_then(|b| std::str::from_utf8(b).ok())
                == Some("extern")
    });
    found
}

/// Find the bound identifier in an `extern` declaration. For
/// `extern int g_counter;` it's the plain `identifier`; for
/// `extern void func(int);` it's the `identifier` inside the
/// `function_declarator`. Pointer wrappers are skipped.
fn extern_bound_identifier<'a>(decl: Node<'a>) -> Option<Node<'a>> {
    let mut cursor = decl.walk();
    for child in decl.named_children(&mut cursor) {
        match child.kind() {
            "identifier" => return Some(child),
            "function_declarator"
            | "pointer_declarator"
            | "array_declarator"
            | "init_declarator" => {
                if let Some(found) = extern_bound_identifier(child) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

/// Walk the translation unit and emit `RawImport` entries for the C "named
/// binding" constructs: `typedef` aliases, `#define` macros (object-like
/// and function-like), and `extern` declarations. Include-guard `#define`s
/// are filtered via [`is_include_guard`].
///
/// `RawImport` shape per construct:
/// - `typedef X Y;`        → `{ source: "X", alias: Some("Y"), imported_name: "Y" }`
/// - `#define MAX 100`     → `{ source: "100", alias: Some("MAX"), imported_name: "MAX" }`
/// - `#define ADD(a,b) ..` → `{ source: "(a,b) ..", alias: Some("ADD"), imported_name: "ADD" }`
/// - `extern int g;`       → `{ source: "external", alias: Some("g"), imported_name: "g" }`
fn extract_named_bindings(root: Node<'_>, source: &[u8], imports: &mut Vec<RawImport>) {
    let mut cursor = root.walk();
    let mut stack: Vec<Node<'_>> = root.named_children(&mut cursor).collect();
    while let Some(node) = stack.pop() {
        match node.kind() {
            "type_definition" => emit_typedef_binding(node, source, imports),
            "preproc_def" => emit_object_macro_binding(node, source, imports),
            "preproc_function_def" => emit_function_macro_binding(node, source, imports),
            "declaration" => emit_extern_binding(node, source, imports),
            // Descend into conditional preproc blocks so guarded defines
            // (and the typical pattern of `#ifdef X / typedef ... / #endif`)
            // still get visited.
            "preproc_if" | "preproc_ifdef" | "preproc_else" | "preproc_elif" => {
                let mut c = node.walk();
                stack.extend(node.named_children(&mut c));
            }
            _ => {}
        }
    }
}

/// Classify an object-like `#define` body into a `BindingKind`.
///
/// Rules (in priority order):
/// 1. Empty body → `Flag`
/// 2. Numeric literal (decimal, hex, float, suffixed) or string literal → `Constant`
/// 3. Single C identifier → `Alias`
/// 4. Everything else (operators, parenthesized expressions, etc.) → `Macro`
fn classify_define_body(body: &str) -> BindingKind {
    let body = body.trim();
    if body.is_empty() {
        return BindingKind::Flag;
    }
    // Numeric literal: decimal/float with optional suffix, or hex.
    let is_numeric = {
        let s = body.trim_start_matches('-');
        (s.starts_with("0x") || s.starts_with("0X"))
            && s[2..]
                .trim_end_matches(['u', 'U', 'l', 'L'])
                .chars()
                .all(|c| c.is_ascii_hexdigit())
            || {
                let stripped = s.trim_end_matches(['u', 'U', 'l', 'L', 'f', 'F']);
                let mut parts = stripped.splitn(2, '.');
                let integer_part = parts.next().unwrap_or("");
                let frac_part = parts.next().unwrap_or("");
                !integer_part.is_empty()
                    && integer_part.chars().all(|c| c.is_ascii_digit())
                    && frac_part.chars().all(|c| c.is_ascii_digit())
            }
    };
    if is_numeric {
        return BindingKind::Constant;
    }
    // String literal.
    if body.starts_with('"') && body.ends_with('"') && body.len() >= 2 {
        return BindingKind::Constant;
    }
    // Single C identifier.
    if body
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '_')
        .unwrap_or(false)
        && body.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return BindingKind::Alias;
    }
    BindingKind::Macro
}

fn emit_typedef_binding(td: Node<'_>, source: &[u8], imports: &mut Vec<RawImport>) {
    // Children: `typedef` keyword, type spec(s), declarator (alias), `;`.
    // The alias is the type_identifier nested in the last named child that
    // isn't the type spec — but the type spec can itself be a `struct_specifier`
    // with a nested `type_identifier` for the struct tag. Strategy: take the
    // last named child whose kind is a declarator family or a bare
    // `type_identifier`.
    let mut cursor = td.walk();
    let named: Vec<Node<'_>> = td.named_children(&mut cursor).collect();
    let Some(alias_root) = named.iter().rev().find(|n| {
        matches!(
            n.kind(),
            "type_identifier"
                | "pointer_declarator"
                | "array_declarator"
                | "function_declarator"
                | "parenthesized_declarator"
        )
    }) else {
        return;
    };
    let Some(alias_node) = find_typedef_alias_name(*alias_root) else {
        return;
    };
    let Ok(alias) = std::str::from_utf8(&source[alias_node.start_byte()..alias_node.end_byte()])
    else {
        return;
    };

    // Underlying type text = slice from just after `typedef` keyword up to
    // the alias declarator's start. This preserves the original spelling
    // including `struct foo { ... }`, function-pointer return types, etc.
    let typedef_kw_end = {
        let mut c = td.walk();
        let kw_end = td
            .children(&mut c)
            .find(|ch| ch.kind() == "typedef")
            .map(|ch| ch.end_byte())
            .unwrap_or(td.start_byte());
        kw_end
    };
    // For function-pointer typedefs the alias is inside the declarator, so
    // the underlying type ends at the declarator's start. For plain typedefs
    // it ends at the alias `type_identifier`'s start.
    let underlying_end = alias_root.start_byte();
    let underlying = std::str::from_utf8(&source[typedef_kw_end..underlying_end])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "<unknown>".to_string());

    imports.push(driver::imports::raw(
        underlying,
        alias.to_string(),
        Some(alias.to_string()),
        Some(BindingKind::Alias),
    ));
}

fn emit_object_macro_binding(def: Node<'_>, source: &[u8], imports: &mut Vec<RawImport>) {
    let mut cursor = def.walk();
    let mut name: Option<&str> = None;
    let mut body: Option<&str> = None;
    for child in def.named_children(&mut cursor) {
        match child.kind() {
            "identifier" if name.is_none() => {
                name = std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok();
            }
            "preproc_arg" => {
                body = std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                    .ok()
                    .map(str::trim);
            }
            _ => {}
        }
    }
    let Some(name) = name else { return };
    if is_include_guard(name, body) {
        return;
    }
    let body_str = body.unwrap_or("");
    let kind = classify_define_body(body_str);
    imports.push(driver::imports::raw(
        body_str.to_string(),
        name.to_string(),
        Some(name.to_string()),
        Some(kind),
    ));
}

fn emit_function_macro_binding(def: Node<'_>, source: &[u8], imports: &mut Vec<RawImport>) {
    let mut cursor = def.walk();
    let mut name: Option<&str> = None;
    let mut params: Option<&str> = None;
    let mut body: Option<&str> = None;
    for child in def.named_children(&mut cursor) {
        match child.kind() {
            "identifier" if name.is_none() => {
                name = std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok();
            }
            "preproc_params" => {
                params = std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok();
            }
            "preproc_arg" => {
                body = std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                    .ok()
                    .map(str::trim);
            }
            _ => {}
        }
    }
    let Some(name) = name else { return };
    // `source` carries `(params) body` so call sites can distinguish a
    // function-like macro from an object-like one without an extra flag.
    let combined = match (params, body) {
        (Some(p), Some(b)) => format!("{p} {b}"),
        (Some(p), None) => p.to_string(),
        (None, Some(b)) => b.to_string(),
        (None, None) => String::new(),
    };
    imports.push(driver::imports::raw(
        combined,
        name.to_string(),
        Some(name.to_string()),
        Some(BindingKind::Macro),
    ));
}

fn emit_extern_binding(decl: Node<'_>, source: &[u8], imports: &mut Vec<RawImport>) {
    if extern_specifier(decl, source).is_none() {
        return;
    }
    let Some(id_node) = extern_bound_identifier(decl) else {
        return;
    };
    let Ok(name) = std::str::from_utf8(&source[id_node.start_byte()..id_node.end_byte()]) else {
        return;
    };
    imports.push(driver::imports::raw(
        "external".to_string(),
        name.to_string(),
        Some(name.to_string()),
        Some(BindingKind::Alias),
    ));
}

pub struct CProvider {
    compiled: Compiled<CSpec>,
}

impl CProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
        })
    }
}

impl LanguageProvider for CProvider {
    fn name(&self) -> &'static str {
        "c"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        driver::parse(&self.compiled, path, source)
    }
}

/// The captures C handles itself. A root capture sets the declaration
/// root; the `.name` kind capture of the same match sets name and kind.
#[derive(Clone, Copy)]
pub enum CRole {
    Root,
    Type,
    ImportSource,
    Field,
    FieldName,
    Var,
    VarName,
}

#[derive(Clone, Copy)]
pub enum CHook {
    Calls,
    Indirect,
    NamedBindings,
    MacroFallback,
    ReceiverOwners,
}

/// Each declaration captures both the outer node (for span) and the
/// identifier (for name + type-slice end byte).
#[derive(Default)]
pub struct CMatch<'t> {
    type_node: Option<Node<'t>>,
    import_src: Option<Node<'t>>,
    field_root: Option<Node<'t>>,
    field_name: Option<Node<'t>>,
    var_root: Option<Node<'t>>,
    var_name: Option<Node<'t>>,
}

#[derive(Default)]
pub struct CFile {
    /// Written by the calls step, read by the receiver-owners step.
    methods: CReceiverMap,
}

impl ParseSpec for CSpec {
    type Role = CRole;
    type Match<'t> = CMatch<'t>;
    type File<'t> = CFile;
    type Hook = CHook;

    const KIND_CAPTURE: KindCapture = KindCapture::NameOverwrite;
    /// P7 covers C dispatch sites that `indirect_dispatch.rs` doesn't
    /// already flag as CallMeta (function-pointer invocation is already
    /// covered there).
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[(
        "blind.dlsym",
        (
            "c-dlsym",
            "dlsym(<handle>, <name>) — runtime symbol resolution from a dlopen'd library; the returned function pointer's target is not statically determinable",
        ),
    )];
    const FIELD_READS: Option<FieldReads> = Some(FieldReads {
        member_kinds: &["field_expression"],
        names: NameRefSpec {
            ident_kinds: &["identifier"],
            kind: NameRefKind::Value,
            skip_kinds: &[
                "preproc_include",
                "preproc_def",
                "preproc_function_def",
                "preproc_call",
                "attribute_specifier",
            ],
            ..NameRefSpec::EMPTY
        },
    });
    const VARIANT_REFS: Option<VariantRefSpec> = Some(VariantRefSpec {
        // C names enumerators bare: `return RED;`, `case GREEN:`.
        path_kinds: &[],
        label_parents: &["case_statement"],
        bare_label_parents: &[],
        bare_expressions: true,
        token_trees: false,
    });
    /// C has no bind step, so the macro-fallback nodes may follow the calls.
    const STEPS: &'static [Step<CHook>] = &[
        Step::fixed(Pass::Query, Op::Query),
        Step::hook(Pass::Calls, Effect::Nodes, CHook::Calls),
        Step::fixed(Pass::Walk, Op::FieldReads),
        Step::fixed(Pass::Walk, Op::VariantRefs),
        Step::hook(Pass::Walk, Effect::Neutral, CHook::Indirect),
        Step::hook(Pass::Imports, Effect::Neutral, CHook::NamedBindings),
        Step::hook(Pass::Walk, Effect::Nodes, CHook::MacroFallback),
        Step::hook(Pass::Build, Effect::Nodes, CHook::ReceiverOwners),
        Step::fixed(Pass::Walk, Op::FunctionMeta),
        Step::fixed(Pass::Build, Op::StampOwners),
    ];

    fn query() -> anyhow::Result<Query> {
        Ok(Query::new(
            &tree_sitter_c::LANGUAGE.into(),
            include_str!("queries.scm"),
        )?)
    }

    fn role(capture: &str) -> Option<CRole> {
        Some(match capture {
            "function" | "struct" | "union" | "enum" | "enumerator_node" | "typedef" | "macro" => {
                CRole::Root
            }
            "type" => CRole::Type,
            "import.source" => CRole::ImportSource,
            "field" => CRole::Field,
            "field.name" => CRole::FieldName,
            "var" => CRole::Var,
            "var.name" => CRole::VarName,
            _ => return None,
        })
    }

    fn parse_tree(source: &[u8]) -> Option<Tree> {
        PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
    }

    fn capture<'t>(
        r: CRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut CMatch<'t>,
        _f: &mut CFile,
        _cx: &mut FileCx<'t>,
    ) {
        match r {
            CRole::Root => d.root = Some(node),
            CRole::Type => m.type_node = Some(node),
            CRole::ImportSource => m.import_src = Some(node),
            CRole::Field => m.field_root = Some(node),
            CRole::FieldName => m.field_name = Some(node),
            CRole::Var => m.var_root = Some(node),
            CRole::VarName => m.var_name = Some(node),
        }
    }

    fn end_match<'t>(d: DeclSlot<'t>, m: &mut CMatch<'t>, _f: &mut CFile, cx: &mut FileCx<'t>) {
        let source = cx.source;
        let CMatch {
            type_node,
            import_src,
            field_root,
            field_name,
            var_root,
            var_name,
        } = std::mem::take(m);

        if let (Some(n), Some(k), Some(root)) = (d.name, d.kind, d.root) {
            // K&R-style multi-line function decl can confuse tree-sitter-c:
            //   XXH_PUBLIC_API XXH_errorcode
            //   XXH64_update(int x, int y) { ... }
            // gets parsed as function_declarator with `XXH_errorcode` as
            // the first identifier (captured by @function.name) and the
            // real name `XXH64_update` wrapped in an ERROR sibling.
            // Walk into the next ERROR sibling for the real identifier.
            let recovered_name = if matches!(k, NodeKind::Function | NodeKind::Method) {
                n.next_sibling().and_then(|err| {
                    if err.kind() != "ERROR" {
                        return None;
                    }
                    // Two-step extraction: copy out primitive byte offsets
                    // while cursor is alive, then re-borrow source outside.
                    let mut cur = err.walk();
                    let bytes = err
                        .children(&mut cur)
                        .find(|c| c.kind() == "identifier")
                        .map(|id| (id.start_byte(), id.end_byte()));
                    bytes.and_then(|(s, e)| {
                        std::str::from_utf8(&source[s..e]).ok().map(String::from)
                    })
                })
            } else {
                None
            };
            let name_owned: Option<String> = recovered_name.or_else(|| {
                std::str::from_utf8(&source[n.start_byte()..n.end_byte()])
                    .ok()
                    .map(|s| s.to_string())
            });
            if let Some(name_str) = name_owned {
                let type_annotation = type_node.and_then(|t| {
                    std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                        .ok()
                        .map(|s| s.trim().to_string())
                });

                // A function with `static` storage class is translation-unit private.
                let is_exported = !has_static_specifier(root, source);

                let decorators = if matches!(k, NodeKind::Function | NodeKind::Method) {
                    crate::framework_helpers::collect_cpp_attributes(root, source)
                } else {
                    vec![]
                };

                cx.nodes.push(RawNode {
                    decorators,
                    is_exported,
                    type_annotation,
                    ..node::decl(
                        name_str,
                        k,
                        node::span(root),
                        &source[root.start_byte()..root.end_byte()],
                    )
                });
            }
        }

        // Struct / union field → Property node with type slice.
        if let (Some(f_root), Some(f_name)) = (field_root, field_name) {
            if let Ok(name_str) =
                std::str::from_utf8(&source[f_name.start_byte()..f_name.end_byte()])
            {
                cx.nodes.push(RawNode {
                    is_exported: true,
                    type_annotation: slice_type_before(f_root, f_name, source),
                    ..node::decl(
                        name_str.to_string(),
                        NodeKind::Property,
                        node::span(f_root),
                        &source[f_root.start_byte()..f_root.end_byte()],
                    )
                });
            }
        }

        // Top-level variable / const declaration → Variable node.
        // Type slice includes storage-class + qualifiers (see
        // `slice_type_before` doc comment).
        if let (Some(v_root), Some(v_name)) = (var_root, var_name) {
            if let Ok(name_str) =
                std::str::from_utf8(&source[v_name.start_byte()..v_name.end_byte()])
            {
                // tree-sitter-c can mis-parse multi-line preprocessor
                // macros (verified on ziplist.c lines 408-443 where the
                // `ZIP_DECODE_LENGTH` `do{...}while(0)` macro causes the
                // following function decl to be partially re-parsed as
                // `(translation_unit (declaration ...))`, capturing
                // function parameters AND type keywords as @var.name).
                // Two-layer guard:
                //   1. v_root.has_error() — the declaration node itself
                //      (or any descendant) is an ERROR/MISSING node, so
                //      the @var capture is recovery noise. Real var
                //      decls in well-formed code have has_error=false.
                //   2. C reserved keyword check — defensive even when
                //      has_error is somehow false; `unsigned`/`const`/
                //      etc. are never legal C variable names.
                if !v_root.has_error() && !is_c_reserved_keyword(name_str) {
                    cx.nodes.push(RawNode {
                        is_exported: true,
                        type_annotation: slice_type_before(v_root, v_name, source),
                        ..node::decl(
                            name_str.to_string(),
                            NodeKind::Variable,
                            node::span(v_root),
                            &source[v_root.start_byte()..v_root.end_byte()],
                        )
                    });
                }
            }
        }

        if let Some(i_src) = import_src {
            if let Ok(src_str) = std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
            {
                cx.imports.push(driver::imports::raw(
                    src_str.to_string(),
                    "*".to_string(),
                    None,
                    None,
                ));
            }
        }
    }

    fn hook<'t>(h: CHook, _c: &Compiled<Self>, f: &mut CFile, cx: &mut FileCx<'t>) {
        let root = cx.tree.root_node();
        let source = cx.source;
        match h {
            CHook::Calls => {
                // Extract call sites with C-convention receiver binding: functions
                // taking a `(struct T *self, ...)`-shaped first param are treated
                // as methods on `T`, so call sites rewrite to `T.fn` for the
                // resolver's Tier 2.5 qualifier-scoped lookup. Convention-driven,
                // not language-mandated — see `RECEIVER_NAMES` for the gate.
                f.methods = collect_receiver_methods(root, source);
                let (raw_path_literals, raw_sql_refs) = extract_c_calls_and_path_literals(
                    root,
                    source,
                    &mut cx.nodes,
                    &f.methods,
                    &mut cx.call_sites,
                );
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
            }
            CHook::Indirect => {
                let fn_ptr_vars = collect_c_cpp_fn_ptr_vars(root, source);
                cx.call_metas = detect_c_cpp_indirect(
                    root,
                    source,
                    &cx.nodes,
                    &fn_ptr_vars,
                    false,
                    &cx.call_sites,
                );
            }
            // Named bindings: `typedef`, `#define`, `extern` declarations.
            // Emitted as `RawImport` with `alias = Some(short_name)` mirroring
            // Java's static-import convention so downstream resolvers can
            // qualifier-scope-lookup C aliases the same way.
            CHook::NamedBindings => extract_named_bindings(root, source, &mut cx.imports),
            // `#define NAME` regex fallback — tree-sitter-c (0.24.x) ERROR-
            // recovers around multi-line `\` continuations + `##` token-paste
            // and drops the `preproc_def` wrapper, leaving the (preproc_def …)
            // query empty for those regions. The fallback scans raw source as
            // a safety net and emits Macro nodes for any `#define NAME` not
            // already captured. Verified `.sample_repo`: ecp went from 11/29
            // macros in `tsd.h` to full recall after this pass.
            CHook::MacroFallback => emit_macro_fallback(source, &mut cx.nodes),
            CHook::ReceiverOwners => {
                // Stamp owner_class from the receiver-convention map (self/this
                // first-param pattern). C has no class syntax; the receiver
                // convention is the closest proxy for struct membership.
                for node in cx.nodes.iter_mut() {
                    if matches!(node.kind, NodeKind::Function | NodeKind::Method) {
                        if let Some(recv_ty) = f.methods.get(&node.name) {
                            node.owner_class = Some(recv_ty.to_string());
                        }
                    }
                }
            }
        }
    }

    fn function_meta(
        root: Node<'_>,
        source: &[u8],
        nodes: &[RawNode],
        category: FileCategory,
    ) -> Vec<RawFunctionMeta> {
        crate::function_meta::c::extract(root, source, nodes, category)
    }
}

/// Augment `nodes` with `#define NAME` Macros that tree-sitter ERROR-
/// recovery dropped. See `preproc_fallback` module docs for the failure
/// shape this covers.
fn emit_macro_fallback(source: &[u8], nodes: &mut Vec<RawNode>) {
    let existing: std::collections::HashSet<String> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Macro)
        .map(|n| n.name.clone())
        .collect();
    for hit in crate::preproc_fallback::scan_define_macros(source) {
        if existing.contains(&hit.name) {
            continue;
        }
        nodes.push(RawNode {
            decorators: vec![],
            is_exported: true,
            heritage: vec![],
            type_annotation: None,
            name: hit.name,
            kind: NodeKind::Macro,
            span: (hit.line, hit.col_start, hit.line, hit.col_end),
            calls: Vec::new(),
            field_reads: Vec::new(),
            owner_class: None,
            content_hash: 0,
        });
    }
}
