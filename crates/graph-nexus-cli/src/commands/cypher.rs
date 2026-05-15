use crate::engine::Engine;
use crate::repo_selector;
use clap::Args;
use graph_nexus_core::graph::{ArchivedZeroCopyGraph, NodeKind, RelType};
use graph_nexus_core::registry::RegistryFile;
use regex::Regex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;

static CYPHER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)MATCH\s+\((?P<src_var>\w+):(?P<src_kind>\w+)\)\s*<*-\[\s*(?P<rel_var>\w*)?(?::(?P<rel_type>\w+))?\s*(?P<star>\*)?\s*(?P<min_len>\d*)?\s*(?:\.\.\s*(?P<max_len>\d*)?)?\s*\]->*\s*\((?P<tgt_var>\w+):(?P<tgt_kind>\w+)\)\s*(?:WHERE\s+(?P<where_var>\w+)\.(?P<where_prop>\w+)\s*=\s*'(?P<where_val>[^']+)')?\s*RETURN\s+(?P<ret>[\w\.,\s]+)"
    ).expect("Failed to compile Cypher regex")
});

#[derive(Args, Debug, Clone)]
pub struct CypherArgs {
    /// The Cypher query string. Accepts the positional form
    /// (`gnx cypher "MATCH ..."`) — the `--query` named form below
    /// stays as an alias for parity with old MCP / wrapper habits.
    #[arg(value_name = "QUERY")]
    pub query_positional: Option<String>,

    /// Named alias for the positional QUERY argument.
    #[arg(
        long = "query",
        value_name = "QUERY",
        conflicts_with = "query_positional"
    )]
    pub query: Option<String>,

    /// Repository to query. Cypher operates on a single graph (single-repo only).
    /// If --repo resolves to multiple repos, an error is returned.
    #[arg(long)]
    pub repo: Option<String>,

    #[arg(long, default_value = "json")]
    pub format: String,
}

impl CypherArgs {
    /// Resolve query from either positional or `--query` form. Returns an
    /// `InvalidArgument` error if neither was supplied — mirrors the prior
    /// behaviour where clap rejected a missing positional outright.
    fn resolved_query(&self) -> Result<&str, graph_nexus_core::GnxError> {
        self.query
            .as_deref()
            .or(self.query_positional.as_deref())
            .ok_or_else(|| {
                graph_nexus_core::GnxError::InvalidArgument(
                    "cypher requires a query — pass it positionally (gnx cypher \"MATCH ...\") or via --query".into(),
                )
            })
    }
}

/// Lazily read + cache file bodies during a single cypher query. We may emit
/// `content` for many nodes that share the same file, so we read each file at
/// most once and keep the contents in-memory for the lifetime of the query.
/// Missing / unreadable files are stored as `None` so the next miss skips the
/// syscall too.
struct ContentCache {
    repo_root: PathBuf,
    files: HashMap<usize, Option<String>>,
}

impl ContentCache {
    fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            files: HashMap::new(),
        }
    }

    /// Resolve `graph.files[file_idx]` to the absolute on-disk path and read
    /// it once. Returns `None` on any I/O failure (file deleted between
    /// `analyze` and the query, perms, etc.) — callers fall back to an empty
    /// `content` string so we never panic on a stale graph.
    fn body_for_node(&mut self, graph: &ArchivedZeroCopyGraph, file_idx: usize) -> Option<&str> {
        if !self.files.contains_key(&file_idx) {
            let body = if file_idx < graph.files.len() {
                let rel = graph.files[file_idx].path.resolve(&graph.string_pool);
                let abs = self.repo_root.join(rel);
                std::fs::read_to_string(&abs).ok()
            } else {
                None
            };
            self.files.insert(file_idx, body);
        }
        self.files.get(&file_idx).and_then(|v| v.as_deref())
    }
}

/// Slice `source` by a tree-sitter style `(start_row, start_col, end_row,
/// end_col)` span. Rows/cols are 0-indexed; columns count UTF-8 bytes (which
/// is what tree-sitter emits). Returns an empty string when the span falls
/// outside the file — the graph may be stale relative to the file on disk
/// and we don't want a query to panic for one bad row.
fn slice_by_span(source: &str, span: (u32, u32, u32, u32)) -> String {
    let (start_row, start_col, end_row, end_col) = (
        span.0 as usize,
        span.1 as usize,
        span.2 as usize,
        span.3 as usize,
    );
    let lines: Vec<&str> = source.split('\n').collect();
    if start_row >= lines.len() || end_row >= lines.len() || start_row > end_row {
        return String::new();
    }

    if start_row == end_row {
        let line = lines[start_row].as_bytes();
        if start_col > line.len() || end_col > line.len() || start_col > end_col {
            return String::new();
        }
        return String::from_utf8_lossy(&line[start_col..end_col]).into_owned();
    }

    let mut out = String::new();
    // First line: drop bytes before start_col.
    let first = lines[start_row].as_bytes();
    let start_col = start_col.min(first.len());
    out.push_str(&String::from_utf8_lossy(&first[start_col..]));
    out.push('\n');
    // Middle lines: full.
    for line in &lines[start_row + 1..end_row] {
        out.push_str(line);
        out.push('\n');
    }
    // Last line: keep bytes before end_col.
    let last = lines[end_row].as_bytes();
    let end_col = end_col.min(last.len());
    out.push_str(&String::from_utf8_lossy(&last[..end_col]));
    out
}

/// Which of the three bound variables in the MATCH clause the RETURN clause
/// requested `.content` for. Anything else in RETURN is ignored — the parser
/// only flips these flags, so plain `RETURN a, b` keeps the legacy shape.
#[derive(Default, Debug, Clone, Copy)]
struct ContentRequests {
    src: bool,
    tgt: bool,
    /// Reserved for edge.content — currently edges have no body, but tracking
    /// the flag here means we won't silently swallow the request if anyone
    /// later adds a meaning for it.
    rel: bool,
}

impl ContentRequests {
    fn parse(ret: &str, src_var: &str, tgt_var: &str, rel_var: &str) -> Self {
        let mut out = Self::default();
        for token in ret.split(',') {
            let token = token.trim();
            let Some((var, prop)) = token.split_once('.') else {
                continue;
            };
            if prop.trim() != "content" {
                continue;
            }
            let var = var.trim();
            if var == src_var {
                out.src = true;
            } else if var == tgt_var {
                out.tgt = true;
            } else if !rel_var.is_empty() && var == rel_var {
                out.rel = true;
            }
        }
        out
    }
}

/// Resolve the repo root used to read source files. Mirrors the `repo_opt`
/// fallback in `main.rs`: `--repo` arg wins, otherwise the current working
/// directory (analyze stored file paths relative to that root).
fn resolve_repo_root(repo_arg: Option<&str>) -> PathBuf {
    if let Some(r) = repo_arg {
        return PathBuf::from(r);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Read the body text for `node_idx` and attach it as `content` on `block`.
/// `block` must already be a JSON object — we insert in-place rather than
/// rebuild so the existing shape of the source/target objects is preserved.
/// Stale / missing files give an empty string; we never panic on bad spans.
fn attach_content(
    block: &mut serde_json::Value,
    graph: &ArchivedZeroCopyGraph,
    node_idx: usize,
    cache: &mut ContentCache,
) {
    let node = &graph.nodes[node_idx];
    let file_idx = node.file_idx.to_native() as usize;
    let span = (
        node.span.0.to_native(),
        node.span.1.to_native(),
        node.span.2.to_native(),
        node.span.3.to_native(),
    );
    let body = cache
        .body_for_node(graph, file_idx)
        .map(|src| slice_by_span(src, span))
        .unwrap_or_default();
    if let Some(obj) = block.as_object_mut() {
        obj.insert("content".to_string(), serde_json::Value::String(body));
    }
}

pub fn run(args: CypherArgs, engine: &Engine) -> Result<(), graph_nexus_core::GnxError> {
    // Multi-repo gate: cypher is single-repo only (graph identity is per-repo).
    if let Some(repo_sel) = args.repo.as_deref() {
        let home_gnx = graph_nexus_core::registry::resolve_home_gnx();
        let registry =
            RegistryFile::read_or_empty(&home_gnx.join("registry.json")).map_err(|e| {
                graph_nexus_core::GnxError::InvalidArgument(format!("registry read: {e}"))
            })?;
        let selector = repo_selector::parse(repo_sel).map_err(|e| {
            graph_nexus_core::GnxError::InvalidArgument(format!("--repo selector: {e}"))
        })?;
        let cwd = std::env::current_dir().unwrap_or_default();
        let repos = repo_selector::resolve(&selector, &registry, cwd.to_str().unwrap_or("."))
            .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("--repo: {e}")))?;
        if repos.len() > 1 {
            return Err(graph_nexus_core::GnxError::InvalidArgument(format!(
                "cypher is single-repo only (graph identity); --repo resolved to {} repos. Pick one with --repo <name|path>.",
                repos.len()
            )));
        }
    }

    let graph = engine
        .graph()
        .map_err(|e| graph_nexus_core::GnxError::Rkyv(e.to_string()))?;

    let query_str = args.resolved_query()?;
    let caps = match CYPHER_REGEX.captures(query_str) {
        Some(c) => c,
        None => {
            return Err(graph_nexus_core::GnxError::InvalidArgument(
                "Query not supported. Minimal Cypher supports: MATCH (a:Kind)-[r:Rel]->(b:Kind) [WHERE a.name='Val'] RETURN a,b".to_string(),
            ));
        }
    };

    let src_var = caps.name("src_var").map(|m| m.as_str()).unwrap_or("");
    let tgt_var = caps.name("tgt_var").map(|m| m.as_str()).unwrap_or("");
    let rel_var = caps.name("rel_var").map(|m| m.as_str()).unwrap_or("");
    let ret = caps.name("ret").map(|m| m.as_str()).unwrap_or("");
    let content_req = ContentRequests::parse(ret, src_var, tgt_var, rel_var);
    let _ = content_req.rel; // rel.content reserved — no body source for edges today
    let mut cache = ContentCache::new(resolve_repo_root(args.repo.as_deref()));

    let src_kind: Option<NodeKind> = caps.name("src_kind").and_then(|m| m.as_str().parse().ok());
    let tgt_kind: Option<NodeKind> = caps.name("tgt_kind").and_then(|m| m.as_str().parse().ok());
    let rel_type: Option<RelType> = caps.name("rel_type").and_then(|m| m.as_str().parse().ok());

    let is_variable = caps.name("star").is_some();
    let min_len: usize = caps
        .name("min_len")
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(1);
    let max_len: usize = caps
        .name("max_len")
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(if is_variable { usize::MAX } else { 1 });

    let where_clause = caps.name("where_var").map(|var| {
        (
            var.as_str(),
            caps.name("where_prop").unwrap().as_str(),
            caps.name("where_val").unwrap().as_str(),
        )
    });

    let mut results = Vec::new();

    for (src_idx, node) in graph.nodes.iter().enumerate() {
        if src_kind.is_some_and(|k| node.kind != k) {
            continue;
        }

        let s = graph.out_offsets[src_idx].to_native() as usize;
        let e = graph.out_offsets[src_idx + 1].to_native() as usize;
        let edges_slice = &graph.edges[s..e];

        if edges_slice.is_empty() {
            continue;
        }

        if let Some((_, prop, val)) = where_clause {
            if prop == "name" {
                let name = node.name.resolve(&graph.string_pool);
                if name != val {
                    continue;
                }
            }
        }

        let name = node.name.resolve(&graph.string_pool);

        if !is_variable {
            for edge in edges_slice {
                if rel_type.is_some_and(|req_rel| edge.rel_type != req_rel) {
                    continue;
                }

                let tgt_idx = edge.target.to_native() as usize;
                let tgt_node = &graph.nodes[tgt_idx];

                if tgt_kind.is_some_and(|k| tgt_node.kind != k) {
                    continue;
                }

                let tgt_name = tgt_node.name.resolve(&graph.string_pool);
                let file_idx = tgt_node.file_idx.to_native() as usize;

                let tgt_path = if file_idx < graph.files.len() {
                    graph.files[file_idx].path.resolve(&graph.string_pool)
                } else {
                    ""
                };

                let mut source = serde_json::json!({ "name": name.to_string() });
                let mut target = serde_json::json!({
                    "name": tgt_name.to_string(),
                    "filePath": tgt_path.to_string(),
                    "kind": format!("{:?}", tgt_node.kind),
                });

                if content_req.src {
                    attach_content(&mut source, graph, src_idx, &mut cache);
                }
                if content_req.tgt {
                    attach_content(&mut target, graph, tgt_idx, &mut cache);
                }

                results.push(serde_json::json!({
                    "source": source,
                    "target": target,
                    "edge": {
                        "reason": edge.reason.resolve(&graph.string_pool),
                        "confidence": edge.confidence.to_native(),
                    }
                }));
            }
        } else {
            let callees = graph_nexus_core::graph_query::callees_of(graph, src_idx as u32, max_len);

            for (tgt_idx, depth) in callees {
                if depth < min_len || depth > max_len {
                    continue;
                }

                let tgt_node = &graph.nodes[tgt_idx as usize];

                if tgt_kind.is_some_and(|k| tgt_node.kind != k) {
                    continue;
                }

                let tgt_name = tgt_node.name.resolve(&graph.string_pool);
                let file_idx = tgt_node.file_idx.to_native() as usize;

                let tgt_path = if file_idx < graph.files.len() {
                    graph.files[file_idx].path.resolve(&graph.string_pool)
                } else {
                    ""
                };

                // Variable-length paths aggregate multiple hops; only surface
                // edge reason/confidence when the path is a single hop (depth
                // == 1) and a direct edge exists. Otherwise the per-hop edge
                // is ambiguous so emit nulls to keep the field shape stable.
                let direct_edge = if depth == 1 {
                    edges_slice
                        .iter()
                        .find(|e| e.target.to_native() as usize == tgt_idx as usize)
                } else {
                    None
                };
                let edge_block = match direct_edge {
                    Some(edge) => serde_json::json!({
                        "reason": edge.reason.resolve(&graph.string_pool),
                        "confidence": edge.confidence.to_native(),
                    }),
                    None => serde_json::json!({
                        "reason": serde_json::Value::Null,
                        "confidence": serde_json::Value::Null,
                    }),
                };

                let mut source = serde_json::json!({ "name": name.to_string() });
                let mut target = serde_json::json!({
                    "name": tgt_name.to_string(),
                    "filePath": tgt_path.to_string(),
                    "kind": format!("{:?}", tgt_node.kind),
                });

                if content_req.src {
                    attach_content(&mut source, graph, src_idx, &mut cache);
                }
                if content_req.tgt {
                    attach_content(&mut target, graph, tgt_idx as usize, &mut cache);
                }

                results.push(serde_json::json!({
                    "source": source,
                    "target": target,
                    "depth": depth,
                    "edge": edge_block,
                }));
            }
        }
    }

    let output = serde_json::json!({
        "results": results
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());

    Ok(())
}
