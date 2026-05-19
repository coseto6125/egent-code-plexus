use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_containerfile::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct DockerfileProvider {
    query: Query,
}

impl DockerfileProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_containerfile::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for DockerfileProvider {
    fn name(&self) -> &'static str {
        "dockerfile"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        // Resolve capture indices once before the loop.
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import = self.query.capture_index_for_name("import");
        let idx_entrypoint = self.query.capture_index_for_name("entrypoint");
        let idx_cmd = self.query.capture_index_for_name("cmd");
        let idx_const_name = self.query.capture_index_for_name("const.name");
        let idx_const = self.query.capture_index_for_name("const");
        let idx_arg_name = self.query.capture_index_for_name("arg.name");
        let idx_arg = self.query.capture_index_for_name("arg");

        // Counter for synthesizing unique names when multiple ENTRYPOINT/CMD exist.
        let mut entrypoint_count: u32 = 0;
        let mut cmd_count: u32 = 0;

        while let Some(m) = matches.next() {
            let mut import_src_node = None;
            let mut import_root_node = None;
            let mut entrypoint_node = None;
            let mut cmd_node = None;
            let mut const_name_node = None;
            let mut const_root_node = None;
            let mut arg_name_node = None;
            let mut arg_root_node = None;

            for cap in m.captures {
                let cap_idx = cap.index;
                if Some(cap_idx) == idx_import_source {
                    import_src_node = Some(cap.node);
                } else if Some(cap_idx) == idx_import {
                    import_root_node = Some(cap.node);
                } else if Some(cap_idx) == idx_entrypoint {
                    entrypoint_node = Some(cap.node);
                } else if Some(cap_idx) == idx_cmd {
                    cmd_node = Some(cap.node);
                } else if Some(cap_idx) == idx_const_name {
                    const_name_node = Some(cap.node);
                } else if Some(cap_idx) == idx_const {
                    const_root_node = Some(cap.node);
                } else if Some(cap_idx) == idx_arg_name {
                    arg_name_node = Some(cap.node);
                } else if Some(cap_idx) == idx_arg {
                    arg_root_node = Some(cap.node);
                }
            }

            // FROM → emit import (image_name node text as source).
            // Also emit a synthetic import node so the image is queryable.
            if let (Some(src), Some(root)) = (import_src_node, import_root_node) {
                if let Ok(src_str) = std::str::from_utf8(&source[src.start_byte()..src.end_byte()])
                {
                    // Grab full image_spec text (name + optional tag) for a richer import string.
                    // src is image_name; its parent is image_spec which may include image_tag.
                    let full_image = src
                        .parent()
                        .and_then(|spec| {
                            std::str::from_utf8(&source[spec.start_byte()..spec.end_byte()])
                                .ok()
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_else(|| src_str.to_string());

                    imports.push(RawImport {
                        alias: None,
                        imported_name: full_image.clone(),
                        source: full_image.clone(),
                        binding_kind: None,
                    });

                    // Also emit the image name as a queryable Const node so users can
                    // run `gnx context --name ubuntu` and get a hit.
                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: true,
                        heritage: vec![],
                        type_annotation: None,
                        name: full_image,
                        kind: NodeKind::Const,
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

            // ENTRYPOINT → emit as Function with synthesized name.
            if let Some(node) = entrypoint_node {
                let name = if entrypoint_count == 0 {
                    "entrypoint".to_string()
                } else {
                    format!("entrypoint_{}", entrypoint_count)
                };
                entrypoint_count += 1;

                let start = node.start_position();
                let end = node.end_position();
                nodes.push(RawNode {
                    decorators: vec![],
                    is_exported: true,
                    heritage: vec![],
                    type_annotation: None,
                    name,
                    kind: NodeKind::Function,
                    span: (
                        start.row as u32,
                        start.column as u32,
                        end.row as u32,
                        end.column as u32,
                    ),
                    calls: Vec::new(),
                });
            }

            // CMD → emit as Function with synthesized name.
            if let Some(node) = cmd_node {
                let name = if cmd_count == 0 {
                    "cmd".to_string()
                } else {
                    format!("cmd_{}", cmd_count)
                };
                cmd_count += 1;

                let start = node.start_position();
                let end = node.end_position();
                nodes.push(RawNode {
                    decorators: vec![],
                    is_exported: true,
                    heritage: vec![],
                    type_annotation: None,
                    name,
                    kind: NodeKind::Function,
                    span: (
                        start.row as u32,
                        start.column as u32,
                        end.row as u32,
                        end.column as u32,
                    ),
                    calls: Vec::new(),
                });
            }

            // ENV → emit each variable as a Const node.
            if let (Some(name_node), Some(root)) = (const_name_node, const_root_node) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                {
                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: true,
                        heritage: vec![],
                        type_annotation: None,
                        name: name_str.to_string(),
                        kind: NodeKind::Const,
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

            // ARG → emit each build argument as a Const node.
            if let (Some(name_node), Some(root)) = (arg_name_node, arg_root_node) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                {
                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: true,
                        heritage: vec![],
                        type_annotation: None,
                        name: name_str.to_string(),
                        kind: NodeKind::Const,
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
