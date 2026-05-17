use super::receiver_types::{collect_bindings, extract_cpp_calls};
use crate::framework_confidence;
use crate::framework_helpers::{detect_ast_framework_patterns, FrameworkPatternSpec};
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

/// True if `node` (a `function_definition`) is defined inside a
/// `field_declaration_list`, which means it is an inline class/struct member
/// function. Tree-sitter aliases `constructor_or_destructor_definition` to
/// `function_definition`, so this check also covers constructors and
/// destructors defined inline.
fn is_inline_class_member(node: tree_sitter::Node<'_>) -> bool {
    let mut cursor = node.parent();
    while let Some(p) = cursor {
        match p.kind() {
            "field_declaration_list" => return true,
            // Stop at translation-unit scope or namespace/linkage boundaries.
            "translation_unit" | "namespace_definition" | "linkage_specification" => {
                return false
            }
            _ => cursor = p.parent(),
        }
    }
    false
}

/// True if `name` is a C/C++ reserved keyword that tree-sitter sometimes
/// mis-captures as an identifier during error-recovery from preprocessor
/// macros. Legal C++ code never names a variable with these.
fn is_cpp_reserved_keyword(name: &str) -> bool {
    matches!(
        name,
        "void" | "char" | "short" | "int" | "long" | "float" | "double"
        | "signed" | "unsigned" | "bool" | "wchar_t" | "char8_t" | "char16_t" | "char32_t"
        | "const" | "volatile" | "constexpr" | "consteval" | "constinit" | "mutable"
        | "static" | "extern" | "auto" | "thread_local" | "register" | "inline"
        | "struct" | "union" | "enum" | "class" | "typedef" | "namespace" | "using"
        | "template" | "typename" | "concept" | "requires"
        | "if" | "else" | "for" | "while" | "do" | "switch" | "case"
        | "default" | "break" | "continue" | "return" | "goto" | "sizeof"
        | "new" | "delete" | "throw" | "try" | "catch" | "noexcept"
        | "public" | "private" | "protected" | "virtual" | "override" | "final"
        | "this" | "nullptr" | "true" | "false" | "operator"
        | "and" | "or" | "not" | "xor" | "bitand" | "bitor" | "compl"
    )
}

/// Per upstream `c-cpp.ts:414-431` `cppProvider.astFrameworkPatterns`.
/// Note: upstream's `cProvider` has no `astFrameworkPatterns`, so this is
/// C++-only.
const CPP_FRAMEWORKS: &[FrameworkPatternSpec] = &[FrameworkPatternSpec {
    framework: "qt",
    reason: "qt-macro",
    confidence: framework_confidence::QT_HINT,
    patterns: &[
        "Q_OBJECT",
        "Q_INVOKABLE",
        "Q_PROPERTY",
        "Q_SIGNALS",
        "Q_SLOTS",
        "Q_SIGNAL",
        "Q_SLOT",
        "QWidget",
        "QApplication",
    ],
}];

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_cpp::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}

/// Slice the source between a declaration's start byte and its identifier
/// name's start byte to recover the type-annotation text.
///
/// Convention (documented per task D3 for both C and C++):
/// - **Pointer / reference spacing** is preserved as-written. `char* s`
///   yields `"char*"`; `const std::string& s` yields `"const std::string&"`.
///   Source is the source of truth.
/// - **Qualifier inclusion** is YES — full prefix including storage class
///   (`static`, `extern`) and cv-qualifiers (`const`, `volatile`).
/// - **`auto`** is preserved literally; the analyzer doesn't do type
///   deduction. `auto x = 5;` → `Some("auto")`.
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

pub struct CppProvider {
    query: Query,
}

impl CppProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_cpp::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for CppProvider {
    fn name(&self) -> &'static str {
        "cpp"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        let idx_name_function = self.query.capture_index_for_name("name.function");
        let idx_name_class = self.query.capture_index_for_name("name.class");
        let idx_name_struct = self.query.capture_index_for_name("name.struct");
        let idx_name_method = self.query.capture_index_for_name("name.method");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");
        let idx_export = self.query.capture_index_for_name("export");
        let idx_alias = self.query.capture_index_for_name("alias");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_struct = self.query.capture_index_for_name("struct");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_import = self.query.capture_index_for_name("import");

        let idx_field = self.query.capture_index_for_name("field");
        let idx_field_name = self.query.capture_index_for_name("field.name");
        let idx_var = self.query.capture_index_for_name("var");
        let idx_var_name = self.query.capture_index_for_name("var.name");

        let idx_name_macro = self.query.capture_index_for_name("name.macro");
        let idx_macro = self.query.capture_index_for_name("macro");
        let idx_name_namespace = self.query.capture_index_for_name("name.namespace");
        let idx_namespace = self.query.capture_index_for_name("namespace");
        let idx_name_enum = self.query.capture_index_for_name("name.enum");
        let idx_enum_node = self.query.capture_index_for_name("enum_node");
        let idx_name_typedef = self.query.capture_index_for_name("name.typedef");
        let idx_typedef_node = self.query.capture_index_for_name("typedef_node");

        let is_header = path
            .extension()
            .map(|ext| ext == "h" || ext == "hpp" || ext == "hxx" || ext == "hh")
            .unwrap_or(false);

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut type_node = None;
            let mut heritage_nodes = Vec::new();
            let mut is_exported_by_query = false;

            let mut import_src_node = None;
            let mut import_alias_node = None;
            let mut is_import = false;

            // Buffers for param / field / var declarations (D3 type annotations).
            let mut field_root: Option<tree_sitter::Node<'_>> = None;
            let mut field_name: Option<tree_sitter::Node<'_>> = None;
            let mut var_root: Option<tree_sitter::Node<'_>> = None;
            let mut var_name: Option<tree_sitter::Node<'_>> = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if cap_idx == idx_name_function {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if cap_idx == idx_name_class {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if cap_idx == idx_name_struct {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Struct);
                } else if cap_idx == idx_name_method {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Method);
                } else if cap_idx == idx_name_macro {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Macro);
                } else if cap_idx == idx_name_namespace {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Namespace);
                } else if cap_idx == idx_name_enum {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Enum);
                } else if cap_idx == idx_name_typedef {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Typedef);
                } else if cap_idx == idx_heritage {
                    heritage_nodes.push(cap.node);
                } else if cap_idx == idx_type {
                    type_node = Some(cap.node);
                } else if cap_idx == idx_export {
                    is_exported_by_query = true;
                } else if cap_idx == idx_alias {
                    import_alias_node = Some(cap.node);
                } else if cap_idx == idx_import_source {
                    import_src_node = Some(cap.node);
                } else if cap_idx == idx_function
                    || cap_idx == idx_class
                    || cap_idx == idx_struct
                    || cap_idx == idx_method
                    || cap_idx == idx_macro
                    || cap_idx == idx_namespace
                    || cap_idx == idx_enum_node
                    || cap_idx == idx_typedef_node
                {
                    root_span_node = Some(cap.node);
                } else if cap_idx == idx_import {
                    is_import = true;
                } else if cap_idx == idx_field {
                    field_root = Some(cap.node);
                } else if cap_idx == idx_field_name {
                    field_name = Some(cap.node);
                } else if cap_idx == idx_var {
                    var_root = Some(cap.node);
                } else if cap_idx == idx_var_name {
                    var_name = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    // Promote free-function to Method when the definition is
                    // lexically inside a class/struct body.
                    let k = if k == NodeKind::Function && is_inline_class_member(root) {
                        NodeKind::Method
                    } else {
                        k
                    };

                    let start = root.start_position();
                    let end = root.end_position();

                    let type_annotation = type_node.and_then(|t| {
                        std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                            .ok()
                            .map(|s| s.trim().to_string())
                    });

                    let heritage = heritage_nodes
                        .iter()
                        .filter_map(|h| {
                            std::str::from_utf8(&source[h.start_byte()..h.end_byte()])
                                .ok()
                                .map(|s| s.to_string())
                        })
                        .collect();

                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: is_header || is_exported_by_query,
                        heritage,
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

            // Class / struct data-member → Property node with type slice.
            if let (Some(f_root), Some(f_name)) = (field_root, field_name) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[f_name.start_byte()..f_name.end_byte()])
                {
                    let start = f_root.start_position();
                    let end = f_root.end_position();
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: is_header || is_exported_by_query,
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

            // Top-level variable / `auto` declaration → Variable node.
            // Guard with `has_error` + C++ reserved-keyword check: tree-sitter
            // C/C++ can re-parse function bodies as `(declaration ...)` after
            // recovering from complex preprocessor macros, capturing function
            // parameters and type keywords as @var.name. Real var decls in
            // well-formed code carry has_error=false and never use keywords
            // as identifier names.
            if let (Some(v_root), Some(v_name)) = (var_root, var_name) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[v_name.start_byte()..v_name.end_byte()])
                {
                    if !v_root.has_error() && !is_cpp_reserved_keyword(name_str) {
                        let start = v_root.start_position();
                        let end = v_root.end_position();
                        nodes.push(RawNode {
                            decorators: vec![],
                            is_exported: is_header || is_exported_by_query,
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

            if is_import {
                if let Some(src_node) = import_src_node {
                    if let Ok(src_str) =
                        std::str::from_utf8(&source[src_node.start_byte()..src_node.end_byte()])
                    {
                        let mut src_s = src_str.to_string();
                        if (src_s.starts_with('"') && src_s.ends_with('"'))
                            || (src_s.starts_with('<') && src_s.ends_with('>'))
                        {
                            src_s = src_s[1..src_s.len() - 1].to_string();
                        }

                        let alias = import_alias_node.and_then(|a| {
                            std::str::from_utf8(&source[a.start_byte()..a.end_byte()])
                                .ok()
                                .map(|s| s.to_string())
                        });

                        let imported_name = src_s.clone();

                        imports.push(RawImport {
                            alias,
                            imported_name,
                            source: src_s,
                            binding_kind: None,
                        });
                    }
                }
            }
        }

        imports.sort_by(|a, b| {
            a.imported_name
                .cmp(&b.imported_name)
                .then(a.source.cmp(&b.source))
                .then(a.alias.cmp(&b.alias))
        });
        imports.dedup_by(|a, b| {
            a.imported_name == b.imported_name && a.source == b.source && a.alias == b.alias
        });

        // Extract call sites with receiver-type binding: `this->method()` /
        // `this.method()` → `Class.method`, `Base::method()` → `Base.method`,
        // and typed-var `obj.method()` / `obj->method()` → `Type.method`.
        // Feeds the resolver's Tier 2.5 qualifier-scoped lookup.
        let bindings = collect_bindings(tree.root_node(), source);
        extract_cpp_calls(tree.root_node(), source, &mut nodes, &bindings);

        let framework_refs = detect_ast_framework_patterns(source, CPP_FRAMEWORKS);

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes: vec![],
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs,
            fanout_refs: vec![],
            blind_spots: vec![],
        })
    }
}
