use crate::engine::Engine;
use ecp_core::graph::NodeKind;
use ecp_core::EcpError;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Build the payload for `ecp impact --literal <VALUE>`. Returns a JSON
/// object with the literal value and an array of sites: each site carries
/// file path, line, enclosing function name (if resolved), and the sink
/// classification (`sink:read|confidence:high`, `sink:write|confidence:medium`,
/// `sink:free|confidence:high`, etc.).
pub(super) fn build_literal_payload(value: &str, engine: &Engine) -> Result<Value, EcpError> {
    use ecp_core::graph::ArchivedRelType;

    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;

    // Pre-build (target_node_idx → edge) map for UsesPathLiteral edges in a
    // single O(|edges|) pass. Without this, the per-match `edges.iter().find`
    // below was O(matches × edges) — at ~500k edges and a popular literal
    // matching dozens of sites, that's tens of millions of comparisons and
    // breaches the <30 ms per-query budget.
    let lit_edge: HashMap<u32, &_> = graph
        .edges
        .iter()
        .filter(|e| matches!(e.rel_type, ArchivedRelType::UsesPathLiteral))
        .map(|e| (e.target.to_native(), e))
        .collect();

    let mut sites: Vec<Value> = Vec::new();
    // `nodes_by_kind` walks the v10 CSR (kind_offsets / kind_node_idx) so we
    // touch only PathLiteral entries, not the full nodes vec.
    for idx_u32 in graph.nodes_by_kind(NodeKind::PathLiteral) {
        let idx = idx_u32 as usize;
        let node = &graph.nodes[idx];
        if node.name.resolve(&graph.string_pool) != value {
            continue;
        }
        let file_node = &graph.files[node.file_idx.to_native() as usize];
        let file_path = file_node.path.resolve(&graph.string_pool);

        let (enclosing_name, sink_reason) = lit_edge
            .get(&idx_u32)
            .map(|e| {
                let src_idx = e.source.to_native() as usize;
                let src_name = graph.nodes[src_idx]
                    .name
                    .resolve(&graph.string_pool)
                    .to_string();
                let reason = e.reason.resolve(&graph.string_pool).to_string();
                (Some(src_name), reason)
            })
            .unwrap_or((None, String::new()));

        sites.push(serde_json::json!({
            "file": file_path,
            "line": node.start_line(),
            "col": node.span.1.to_native(),
            "enclosing": enclosing_name,
            "sink_reason": sink_reason,
        }));
    }

    Ok(serde_json::json!({
        "literal": value,
        "site_count": sites.len(),
        "sites": sites,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiteralAccess {
    ReadOnly,
    WriteOnly,
    MixedOrUnknown,
}

#[derive(Debug, Clone)]
struct LiteralSite {
    file: String,
    line: u32,
    col: u32,
    enclosing: Option<String>,
    sink_reason: String,
}

#[derive(Debug, Clone)]
struct LiteralGroup {
    value: String,
    sites: Vec<LiteralSite>,
}

impl LiteralGroup {
    fn access(&self) -> LiteralAccess {
        let mut read_count = 0usize;
        let mut write_count = 0usize;

        for site in &self.sites {
            match sink_kind(&site.sink_reason) {
                Some("read") | Some("open-read") => read_count += 1,
                Some("write") | Some("open-write") => write_count += 1,
                _ => return LiteralAccess::MixedOrUnknown,
            }
        }

        match (read_count, write_count) {
            (r, 0) if r > 0 => LiteralAccess::ReadOnly,
            (0, w) if w > 0 => LiteralAccess::WriteOnly,
            _ => LiteralAccess::MixedOrUnknown,
        }
    }
}

pub fn build_literal_coherence_payload(engine: &Engine) -> Result<Value, EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let groups = collect_literal_groups(graph);
    let candidates = literal_coherence_candidates(&groups);

    Ok(json!({
        "literal_coherence": {
            "candidate_count": candidates.len(),
            "candidates": candidates,
            "rules": {
                "min_similarity": 0.85,
                "same_extension": true,
                "nearby_directory": true,
                "access_split": "read-only vs write-only"
            }
        }
    }))
}

fn collect_literal_groups(graph: &ecp_core::graph::ArchivedZeroCopyGraph) -> Vec<LiteralGroup> {
    use ecp_core::graph::ArchivedRelType;

    let lit_edge: HashMap<u32, &_> = graph
        .edges
        .iter()
        .filter(|e| matches!(e.rel_type, ArchivedRelType::UsesPathLiteral))
        .map(|e| (e.target.to_native(), e))
        .collect();

    let mut groups: HashMap<String, LiteralGroup> = HashMap::new();
    for idx_u32 in graph.nodes_by_kind(NodeKind::PathLiteral) {
        let node = &graph.nodes[idx_u32 as usize];
        let value = node.name.resolve(&graph.string_pool).to_string();
        let file_node = &graph.files[node.file_idx.to_native() as usize];
        let file = file_node.path.resolve(&graph.string_pool).to_string();
        let (enclosing, sink_reason) = lit_edge
            .get(&idx_u32)
            .map(|e| {
                let src_idx = e.source.to_native() as usize;
                let src_name = graph.nodes[src_idx]
                    .name
                    .resolve(&graph.string_pool)
                    .to_string();
                let reason = e.reason.resolve(&graph.string_pool).to_string();
                (Some(src_name), reason)
            })
            .unwrap_or((None, String::new()));

        let group = groups.entry(value.clone()).or_insert_with(|| LiteralGroup {
            value,
            sites: Vec::new(),
        });
        group.sites.push(LiteralSite {
            file,
            line: node.start_line(),
            col: node.span.1.to_native(),
            enclosing,
            sink_reason,
        });
    }

    groups.into_values().collect()
}

fn literal_coherence_candidates(groups: &[LiteralGroup]) -> Vec<Value> {
    const MIN_SIMILARITY: f64 = 0.85;

    let mut out = Vec::new();
    for (i, left) in groups.iter().enumerate() {
        let left_access = left.access();
        if left_access == LiteralAccess::MixedOrUnknown {
            continue;
        }
        for right in groups.iter().skip(i + 1) {
            let right_access = right.access();
            if !matches!(
                (left_access, right_access),
                (LiteralAccess::ReadOnly, LiteralAccess::WriteOnly)
                    | (LiteralAccess::WriteOnly, LiteralAccess::ReadOnly)
            ) {
                continue;
            }
            if !same_extension(&left.value, &right.value) || !nearby_directory(left, right) {
                continue;
            }

            let similarity = literal_similarity(&left.value, &right.value);
            if similarity < MIN_SIMILARITY {
                continue;
            }

            let (reader, writer) = if left_access == LiteralAccess::ReadOnly {
                (left, right)
            } else {
                (right, left)
            };
            out.push(json!({
                "reader_literal": reader.value,
                "writer_literal": writer.value,
                "similarity": similarity,
                "confidence": "high",
                "reader_sites": sites_json(&reader.sites),
                "writer_sites": sites_json(&writer.sites),
            }));
        }
    }
    out.sort_by(|a, b| {
        let a_similarity = a["similarity"].as_f64().unwrap_or(0.0);
        let b_similarity = b["similarity"].as_f64().unwrap_or(0.0);
        b_similarity
            .partial_cmp(&a_similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

fn sink_kind(reason: &str) -> Option<&str> {
    reason.strip_prefix("sink:")?.split('|').next()
}

fn sites_json(sites: &[LiteralSite]) -> Vec<Value> {
    sites
        .iter()
        .map(|site| {
            json!({
                "file": site.file,
                "line": site.line,
                "col": site.col,
                "enclosing": site.enclosing,
                "sink_reason": site.sink_reason,
            })
        })
        .collect()
}

fn same_extension(left: &str, right: &str) -> bool {
    path_extension(left).is_some_and(|ext| Some(ext) == path_extension(right))
}

fn path_extension(path: &str) -> Option<&str> {
    path.rsplit_once('.')
        .map(|(_, ext)| ext)
        .filter(|ext| !ext.is_empty() && !ext.contains('/') && !ext.contains('\\'))
}

fn nearby_directory(left: &LiteralGroup, right: &LiteralGroup) -> bool {
    left.sites.iter().any(|l| {
        let ldir = parent_dir(&l.file);
        right.sites.iter().any(|r| ldir == parent_dir(&r.file))
    })
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

fn literal_similarity(left: &str, right: &str) -> f64 {
    let left_base = basename_without_ext(left).to_ascii_lowercase();
    let right_base = basename_without_ext(right).to_ascii_lowercase();
    normalized_levenshtein(&left_base, &right_base)
        .max(containment_similarity(&left_base, &right_base))
}

fn basename_without_ext(path: &str) -> &str {
    let name = path.rsplit_once('/').map(|(_, n)| n).unwrap_or(path);
    name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name)
}

fn containment_similarity(left: &str, right: &str) -> f64 {
    let (short, long) = if left.len() <= right.len() {
        (left, right)
    } else {
        (right, left)
    };
    if short.len() < 4 {
        return 0.0;
    }
    let boundary_match = long == short
        || long.starts_with(&format!("{short}_"))
        || long.ends_with(&format!("_{short}"))
        || long.contains(&format!("_{short}_"))
        || long.starts_with(&format!("{short}-"))
        || long.ends_with(&format!("-{short}"))
        || long.contains(&format!("-{short}-"));
    if boundary_match {
        0.9
    } else {
        0.0
    }
}

fn normalized_levenshtein(left: &str, right: &str) -> f64 {
    let max_len = left.chars().count().max(right.chars().count());
    if max_len == 0 {
        return 1.0;
    }
    1.0 - (levenshtein(left, right) as f64 / max_len as f64)
}

fn levenshtein(left: &str, right: &str) -> usize {
    let right_chars: Vec<char> = right.chars().collect();
    let mut prev: Vec<usize> = (0..=right_chars.len()).collect();
    let mut curr = vec![0usize; right_chars.len() + 1];

    for (i, lc) in left.chars().enumerate() {
        curr[0] = i + 1;
        for (j, &rc) in right_chars.iter().enumerate() {
            let cost = usize::from(lc != rc);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[right_chars.len()]
}

#[cfg(test)]
mod literal_coherence_tests {
    use super::*;

    fn group(value: &str, file: &str, sink_reason: &str) -> LiteralGroup {
        LiteralGroup {
            value: value.to_string(),
            sites: vec![LiteralSite {
                file: file.to_string(),
                line: 1,
                col: 0,
                enclosing: Some("f".to_string()),
                sink_reason: sink_reason.to_string(),
            }],
        }
    }

    #[test]
    fn coherence_finds_session_meta_split_brain_pair() {
        let groups = vec![
            group(
                "meta.json",
                "src/session/read.rs",
                "sink:read|confidence:high",
            ),
            group(
                "session_meta.json",
                "src/session/write.rs",
                "sink:write|confidence:high",
            ),
        ];

        let candidates = literal_coherence_candidates(&groups);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0]["reader_literal"], "meta.json");
        assert_eq!(candidates[0]["writer_literal"], "session_meta.json");
    }

    #[test]
    fn coherence_rejects_same_access_pairs() {
        let groups = vec![
            group("config.yml", "src/config/a.rs", "sink:read|confidence:high"),
            group(
                "configs.yml",
                "src/config/b.rs",
                "sink:read|confidence:high",
            ),
        ];

        assert!(literal_coherence_candidates(&groups).is_empty());
    }

    #[test]
    fn coherence_rejects_different_extensions() {
        let groups = vec![
            group("Cargo.toml", "src/build/a.rs", "sink:read|confidence:high"),
            group("Cargo.lock", "src/build/b.rs", "sink:write|confidence:high"),
        ];

        assert!(literal_coherence_candidates(&groups).is_empty());
    }

    #[test]
    fn coherence_rejects_unknown_access_mixed_with_read() {
        let mut reader = group(
            "meta.json",
            "src/session/read.rs",
            "sink:read|confidence:high",
        );
        reader.sites.push(LiteralSite {
            file: "src/session/path.rs".to_string(),
            line: 2,
            col: 0,
            enclosing: Some("path".to_string()),
            sink_reason: "sink:join|confidence:medium".to_string(),
        });
        let groups = vec![
            reader,
            group(
                "session_meta.json",
                "src/session/write.rs",
                "sink:write|confidence:high",
            ),
        ];

        assert!(literal_coherence_candidates(&groups).is_empty());
    }
}
