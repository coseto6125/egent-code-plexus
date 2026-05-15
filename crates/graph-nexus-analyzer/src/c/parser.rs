use super::receiver_types::{collect_receiver_methods, extract_c_calls};
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_c::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
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
fn slice_type_before(decl: tree_sitter::Node<'_>, name: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
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
        let idx_type = self.query.capture_index_for_name("type");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_struct = self.query.capture_index_for_name("struct");

        let idx_param = self.query.capture_index_for_name("param");
        let idx_param_name = self.query.capture_index_for_name("param.name");
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
            let mut param_root: Option<tree_sitter::Node<'_>> = None;
            let mut param_name: Option<tree_sitter::Node<'_>> = None;
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
                    kind = Some(NodeKind::Class);
                } else if Some(cap_idx) == idx_type {
                    type_node = Some(cap.node);
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_function || Some(cap_idx) == idx_struct {
                    root_span_node = Some(cap.node);
                } else if Some(cap_idx) == idx_param {
                    param_root = Some(cap.node);
                } else if Some(cap_idx) == idx_param_name {
                    param_name = Some(cap.node);
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

            // Parameter declaration → Variable node with type slice.
            if let (Some(p_root), Some(p_name)) = (param_root, param_name) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[p_name.start_byte()..p_name.end_byte()])
                {
                    let start = p_root.start_position();
                    let end = p_root.end_position();
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: false,
                        heritage: vec![],
                        type_annotation: slice_type_before(p_root, p_name, source),
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

            if let Some(i_src) = import_src {
                if let Ok(src_str) =
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
                {
                    imports.push(RawImport {
                        alias: None,
                        imported_name: "*".to_string(),
                        source: src_str.to_string(),
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
