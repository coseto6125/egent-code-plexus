use crate::calls::extract_calls;
use super::spec::VerilogSpec;
use cgn_core::analyzer::lang_spec::LangSpec;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use cgn_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_verilog::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct VerilogProvider {
    query: Query,
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

impl VerilogProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_verilog::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;

        let capture_names = query.capture_names();
        let capture_kind_by_idx: Vec<Option<NodeKind>> = capture_names
            .iter()
            .map(|name| VerilogSpec::CAPTURE_KIND.get(name).copied())
            .collect();

        Ok(Self { query, capture_kind_by_idx })
    }
}

impl LanguageProvider for VerilogProvider {
    fn name(&self) -> &'static str {
        "verilog"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();

        let idx_import_source = self.query.capture_index_for_name("import.source");

        let idx_class = self.query.capture_index_for_name("class");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_const = self.query.capture_index_for_name("const");
        let idx_import = self.query.capture_index_for_name("import");
        let idx_class_prop = self.query.capture_index_for_name("class_prop");
        let idx_class_prop_visibility =
            self.query.capture_index_for_name("class_prop.visibility");
        let idx_typedef = self.query.capture_index_for_name("typedef");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut import_src = None;
            let mut class_prop_visibility: Option<&[u8]> = None;
            let mut is_class_prop = false;

            for cap in m.captures {
                let cap_idx = cap.index as usize;
                if let Some(k) = self.capture_kind_by_idx.get(cap_idx).and_then(|opt| *opt) {
                    // This is a .name capture with a NodeKind
                    if name_node.is_none() {
                        name_node = Some(cap.node);
                        kind = Some(k);
                    }
                } else if Some(cap_idx as u32) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx as u32) == idx_class_prop_visibility {
                    class_prop_visibility =
                        Some(&source[cap.node.start_byte()..cap.node.end_byte()]);
                } else if Some(cap_idx as u32) == idx_class_prop {
                    root_span_node = Some(cap.node);
                    is_class_prop = true;
                } else if Some(cap_idx as u32) == idx_class
                    || Some(cap_idx as u32) == idx_method
                    || Some(cap_idx as u32) == idx_const
                    || Some(cap_idx as u32) == idx_import
                    || Some(cap_idx as u32) == idx_typedef
                {
                    root_span_node = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
                    // SV class members: `local`/`protected` → private; all else → exported.
                    let is_exported = if is_class_prop {
                        !matches!(
                            class_prop_visibility,
                            Some(b"local") | Some(b"protected")
                        )
                    } else {
                        true
                    };
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported,
                        heritage: vec![],
                        type_annotation: None,
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

        // Verilog uses function_subroutine_call for subroutine invocations inside always/initial blocks.
        extract_calls(
            tree.root_node(),
            source,
            &mut nodes,
            &["function_subroutine_call"],
        );

        Ok(LocalGraph {
            content_hash: [0; 8],
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
