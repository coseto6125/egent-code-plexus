use crate::resolution::index::SymbolTable;
use crate::resolution::resolver::Resolver;
use gnx_core::analyzer::types::LocalGraph;
use gnx_core::graph::{Edge, File, Node, RelType, ZeroCopyGraph};
use gnx_core::pool::StringPool;

pub struct GraphBuilder {
    local_graphs: Vec<LocalGraph>,
    generate_embeddings: bool,
}

impl Default for GraphBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self {
            local_graphs: Vec::new(),
            generate_embeddings: false,
        }
    }

    pub fn with_embeddings(mut self, generate: bool) -> Self {
        self.generate_embeddings = generate;
        self
    }

    pub fn add_graph(&mut self, graph: LocalGraph) {
        self.local_graphs.push(graph);
    }

    pub fn build(self) -> ZeroCopyGraph {
        let mut symbol_table = SymbolTable::new();
        let mut string_pool = StringPool::new();
        let mut nodes = Vec::new();
        let mut files = Vec::new();

        // Pass 1: Register all nodes into SymbolTable and StringPool
        let mut current_node_idx = 0;
        let mut file_idx = 0;
        let mut embed_texts = Vec::new();

        for local_graph in &self.local_graphs {
            let path_str = local_graph.file_path.to_string_lossy().to_string();
            let path_ref = string_pool.add(&path_str);
            
            files.push(File {
                path: path_ref,
                mtime: 0, // In a real implementation, fetch actual mtime
                content_hash: [0; 32],
            });

            for raw_node in &local_graph.nodes {
                symbol_table.register_node(&path_str, &raw_node.name, current_node_idx);

                let uid_str = format!("{:?}:{}:{}", raw_node.kind, path_str, raw_node.name);
                let uid_ref = string_pool.add(&uid_str);
                let name_ref = string_pool.add(&raw_node.name);

                                nodes.push(Node {
                    uid: uid_ref,
                    name: name_ref,
                    file_idx,
                    kind: raw_node.kind,
                    span: raw_node.span,
                });

                if self.generate_embeddings {
                    let mut text_parts = Vec::new();
                    text_parts.push(format!("{:?}: {}", raw_node.kind, raw_node.name));
                    text_parts.push(format!("Path: {}", path_str));
                    if raw_node.is_exported {
                        text_parts.push("Export: true".to_string());
                    }
                    if !raw_node.decorators.is_empty() {
                        text_parts.push(raw_node.decorators.join(" "));
                    }
                    if let Some(ty) = &raw_node.type_annotation {
                        text_parts.push(format!("Type: {}", ty));
                    }
                    if !raw_node.heritage.is_empty() {
                        text_parts.push(format!("Heritage: {}", raw_node.heritage.join(", ")));
                    }
                    embed_texts.push(text_parts.join("
"));
                }

                current_node_idx += 1;
            }
            file_idx += 1;
        }

        // Pass 1.5: Extract Routes
        let mut route_edges = Vec::new();
        let mut current_handler_idx = 0;
        let mut file_idx = 0;
        for local_graph in &self.local_graphs {
            let path_str = local_graph.file_path.to_string_lossy().to_string();
            
            for raw_node in &local_graph.nodes {
                let handler_idx = current_handler_idx;
                
                for dec in &raw_node.decorators {
                    if let Some(detected) = crate::route_detector::detect_from_decorator(dec) {
                        let route_name = format!("{} {}", detected.method, detected.path);
                        let uid_str = format!("Route:{}:{}", path_str, route_name);
                        
                                                let route_idx = nodes.len() as u32;
                        nodes.push(Node {
                            uid: string_pool.add(&uid_str),
                            name: string_pool.add(&route_name),
                            file_idx,
                            kind: gnx_core::graph::NodeKind::Route,
                            span: raw_node.span,
                        });
                        
                        if self.generate_embeddings {
                            embed_texts.push(format!("Route: {}
Path: {}", route_name, path_str));
                        }
                        
                        route_edges.push(Edge {
                            source: handler_idx,
                            target: route_idx,
                            rel_type: RelType::HandlesRoute,
                            confidence: 1.0,
                            reason: string_pool.add("decorator"),
                        });
                    }
                }
                current_handler_idx += 1;
            }

            for raw_route in &local_graph.routes {
                if let Some(detected) = crate::route_detector::detect_from_call(raw_route) {
                    let route_name = format!("{} {}", detected.method, detected.path);
                    let uid_str = format!("Route:{}:{}", path_str, route_name);
                    
                                        let route_idx = nodes.len() as u32;
                    nodes.push(Node {
                        uid: string_pool.add(&uid_str),
                        name: string_pool.add(&route_name),
                        file_idx,
                        kind: gnx_core::graph::NodeKind::Route,
                        span: raw_route.span,
                    });
                    
                    if self.generate_embeddings {
                        embed_texts.push(format!("Route: {}
Path: {}", route_name, path_str));
                    }
                    
                    // Imperative routes might not easily map to a specific handler.
                    // We just register the Route node here.
                }
            }
            file_idx += 1;
        }

        // Pass 2: Resolve imports and build edges
        let resolver = Resolver::new(&symbol_table);
        let mut edges = Vec::new();
        let mut current_node_idx = 0;

        let reason_heritage = string_pool.add("heritage");
        let reason_type = string_pool.add("type_annotation");

        for local_graph in &self.local_graphs {
            for raw_node in &local_graph.nodes {
                // Resolve heritage (base classes, traits)
                for base in &raw_node.heritage {
                    let targets = resolver.resolve_symbol(&local_graph.file_path, base, &local_graph.imports);
                    for (target_id, confidence) in targets {
                        edges.push(Edge {
                            source: current_node_idx,
                            target: target_id,
                            rel_type: RelType::Calls, // Defaulting to Calls for now
                            confidence,
                            reason: reason_heritage.clone(),
                        });
                    }
                }

                // Resolve type annotation
                if let Some(type_ann) = &raw_node.type_annotation {
                    let targets = resolver.resolve_symbol(&local_graph.file_path, type_ann, &local_graph.imports);
                    for (target_id, confidence) in targets {
                        edges.push(Edge {
                            source: current_node_idx,
                            target: target_id,
                            rel_type: RelType::Accesses,
                            confidence,
                            reason: reason_type.clone(),
                        });
                    }
                }

                current_node_idx += 1;
            }
        }

        edges.extend(route_edges);

        // Final pass: Construct CSR (out_offsets and in_offsets)
        // Sort edges by source to build out_offsets easily
        edges.sort_by_key(|e| e.source);

        let num_nodes = nodes.len();
        let mut out_offsets = vec![0; num_nodes + 1];
        for edge in &edges {
            out_offsets[edge.source as usize + 1] += 1;
        }
        for i in 0..num_nodes {
            out_offsets[i + 1] += out_offsets[i];
        }

        // Build in_edge_idx (indices of edges sorted by target)
        let mut in_edge_idx: Vec<u32> = (0..edges.len() as u32).collect();
        in_edge_idx.sort_by_key(|&idx| edges[idx as usize].target);

        let mut in_offsets = vec![0; num_nodes + 1];
        for &idx in &in_edge_idx {
            let edge = &edges[idx as usize];
            in_offsets[edge.target as usize + 1] += 1;
        }
        for i in 0..num_nodes {
            in_offsets[i + 1] += in_offsets[i];
        }

        let embeddings = if self.generate_embeddings && !embed_texts.is_empty() {
            tracing::info!("Generating embeddings for {} nodes...", embed_texts.len());
            if let Ok(embedder) = crate::embeddings::Embedder::new() {
                match embedder.embed(embed_texts) {
                    Ok(embs) => Some(embs),
                    Err(e) => {
                        tracing::warn!("Failed to embed nodes: {}", e);
                        None
                    }
                }
            } else {
                tracing::warn!("Failed to initialize embedder");
                None
            }
        } else {
            None
        };

        ZeroCopyGraph {
            magic: *b"GNX-RS\0\0",
            fingerprint: [0; 32],
            string_pool: string_pool.bytes,
            files,
            nodes,
            edges,
            out_offsets,
            in_offsets,
            in_edge_idx,
            name_index: Vec::new(), // To be implemented if name indexing is needed
            embeddings,
        }
    }
}