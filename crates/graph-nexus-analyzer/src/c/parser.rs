use super::receiver_types::{collect_receiver_methods, extract_c_calls};
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{BindingKind, LocalGraph, RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor};

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
        "void" | "char" | "short" | "int" | "long" | "float" | "double"
        | "signed" | "unsigned" | "_Bool" | "_Complex" | "_Imaginary"
        | "const" | "volatile" | "restrict" | "_Atomic" | "register"
        | "static" | "extern" | "auto" | "_Thread_local"
        | "struct" | "union" | "enum" | "typedef"
        | "if" | "else" | "for" | "while" | "do" | "switch" | "case"
        | "default" | "break" | "continue" | "return" | "goto" | "sizeof"
        | "inline" | "_Static_assert" | "_Alignas" | "_Alignof"
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

    imports.push(RawImport {
        alias: Some(alias.to_string()),
        imported_name: alias.to_string(),
        source: underlying,
        binding_kind: Some(BindingKind::Alias),
    });
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
    imports.push(RawImport {
        alias: Some(name.to_string()),
        imported_name: name.to_string(),
        source: body_str.to_string(),
        binding_kind: Some(kind),
    });
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
    imports.push(RawImport {
        alias: Some(name.to_string()),
        imported_name: name.to_string(),
        source: combined,
        binding_kind: Some(BindingKind::Macro),
    });
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
    imports.push(RawImport {
        alias: Some(name.to_string()),
        imported_name: name.to_string(),
        source: "external".to_string(),
        binding_kind: Some(BindingKind::Alias),
    });
}

pub struct CProvider {
    query: Query,
}

impl CProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_c::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for CProvider {
    fn name(&self) -> &'static str {
        "c"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        let idx_function_name = self.query.capture_index_for_name("function.name");
        let idx_struct_name = self.query.capture_index_for_name("struct.name");
        let idx_union_name = self.query.capture_index_for_name("union.name");
        let idx_enum_name = self.query.capture_index_for_name("enum.name");
        let idx_typedef_name = self.query.capture_index_for_name("typedef.name");
        let idx_macro_name = self.query.capture_index_for_name("macro.name");
        let idx_type = self.query.capture_index_for_name("type");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_struct = self.query.capture_index_for_name("struct");
        let idx_union = self.query.capture_index_for_name("union");
        let idx_enum = self.query.capture_index_for_name("enum");
        let idx_typedef = self.query.capture_index_for_name("typedef");
        let idx_macro = self.query.capture_index_for_name("macro");

        let idx_field = self.query.capture_index_for_name("field");
        let idx_field_name = self.query.capture_index_for_name("field.name");
        let idx_var = self.query.capture_index_for_name("var");
        let idx_var_name = self.query.capture_index_for_name("var.name");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut type_node = None;
            let mut import_src = None;

            // Buffers for param / field / var declarations. Each declaration
            // captures both the outer node (for span) and the identifier
            // (for name + type-slice end byte).
            let mut field_root: Option<tree_sitter::Node<'_>> = None;
            let mut field_name: Option<tree_sitter::Node<'_>> = None;
            let mut var_root: Option<tree_sitter::Node<'_>> = None;
            let mut var_name: Option<tree_sitter::Node<'_>> = None;

            for cap in m.captures {
                let cap_idx = cap.index;
                if Some(cap_idx) == idx_function_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if Some(cap_idx) == idx_struct_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Struct);
                } else if Some(cap_idx) == idx_union_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Struct);
                } else if Some(cap_idx) == idx_enum_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Enum);
                } else if Some(cap_idx) == idx_typedef_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Typedef);
                } else if Some(cap_idx) == idx_macro_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Macro);
                } else if Some(cap_idx) == idx_type {
                    type_node = Some(cap.node);
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_struct
                    || Some(cap_idx) == idx_union
                    || Some(cap_idx) == idx_enum
                    || Some(cap_idx) == idx_typedef
                    || Some(cap_idx) == idx_macro
                {
                    root_span_node = Some(cap.node);
                } else if Some(cap_idx) == idx_field {
                    field_root = Some(cap.node);
                } else if Some(cap_idx) == idx_field_name {
                    field_name = Some(cap.node);
                } else if Some(cap_idx) == idx_var {
                    var_root = Some(cap.node);
                } else if Some(cap_idx) == idx_var_name {
                    var_name = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();

                    let type_annotation = type_node.and_then(|t| {
                        std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                            .ok()
                            .map(|s| s.trim().to_string())
                    });

                    // A function with `static` storage class is translation-unit private.
                    let is_exported = !has_static_specifier(root, source);

                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported,
                        heritage: vec![],
                        type_annotation,
                        name: name_str.to_string(),
                        kind: k,
                        span: (
                            start.row as u32,
                            start.column as u32,
                            end.row as u32,
                            end.column as u32,
                        ),
                        calls: Vec::new(),
                    });
                }
            }

            // Struct / union field → Property node with type slice.
            if let (Some(f_root), Some(f_name)) = (field_root, field_name) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[f_name.start_byte()..f_name.end_byte()])
                {
                    let start = f_root.start_position();
                    let end = f_root.end_position();
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: true,
                        heritage: vec![],
                        type_annotation: slice_type_before(f_root, f_name, source),
                        name: name_str.to_string(),
                        kind: NodeKind::Property,
                        span: (
                            start.row as u32,
                            start.column as u32,
                            end.row as u32,
                            end.column as u32,
                        ),
                        calls: Vec::new(),
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
                        let start = v_root.start_position();
                        let end = v_root.end_position();
                        nodes.push(RawNode {
                            decorators: vec![],
                            is_exported: true,
                            heritage: vec![],
                            type_annotation: slice_type_before(v_root, v_name, source),
                            name: name_str.to_string(),
                            kind: NodeKind::Variable,
                            span: (
                                start.row as u32,
                                start.column as u32,
                                end.row as u32,
                                end.column as u32,
                            ),
                            calls: Vec::new(),
                        });
                    }
                }
            }

            if let Some(i_src) = import_src {
                if let Ok(src_str) =
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
                {
                    imports.push(RawImport {
                        alias: None,
                        imported_name: "*".to_string(),
                        source: src_str.to_string(),
                        binding_kind: None,
                    });
                }
            }
        }

        // Extract call sites with C-convention receiver binding: functions
        // taking a `(struct T *self, ...)`-shaped first param are treated
        // as methods on `T`, so call sites rewrite to `T.fn` for the
        // resolver's Tier 2.5 qualifier-scoped lookup. Convention-driven,
        // not language-mandated — see `RECEIVER_NAMES` for the gate.
        let methods = collect_receiver_methods(tree.root_node(), source);
        extract_c_calls(tree.root_node(), source, &mut nodes, &methods);

        // Named bindings: `typedef`, `#define`, `extern` declarations.
        // Emitted as `RawImport` with `alias = Some(short_name)` mirroring
        // Java's static-import convention so downstream resolvers can
        // qualifier-scope-lookup C aliases the same way.
        extract_named_bindings(tree.root_node(), source, &mut imports);

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes: vec![],
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
        })
    }
}
