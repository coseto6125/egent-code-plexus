//! Process (execution-flow) detection on the in-memory graph.
//!
//! Mirrors upstream `process-processor.ts`:
//!   1. Find entry points (Functions/Methods with callees & low caller count)
//!   2. BFS forward via `RelType::Calls` (confidence >= MIN_TRACE_CONFIDENCE)
//!   3. Collect distinct traces meeting `min_steps`
//!   4. Dedup: subset removal + endpoint pair dedup (keep longest per pair)
//!   5. Cap at `max_processes`
//!
//! Returns `Vec<TraceResult>` ordered by descending step count. Each result's
//! `trace` is a sequence of node indices into the original `nodes` slice.

use crate::graph::{Edge, Node, NodeKind, RelType};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone)]
pub struct ProcessConfig {
    pub max_trace_depth: usize,
    pub max_branching: usize,
    pub max_processes: usize,
    pub min_steps: usize,
    pub min_confidence: f32,
    pub max_entry_points: usize,
}

impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            max_trace_depth: 10,
            max_branching: 4,
            max_processes: 75,
            min_steps: 3,
            min_confidence: 0.5,
            max_entry_points: 200,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TraceResult {
    pub trace: Vec<u32>,           // node indices, ordered entry → terminal
    pub process_type: ProcessType, // cross-community detection (vs intra-)
    pub communities: Vec<u16>,     // unique communities touched
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessType {
    IntraCommunity,
    CrossCommunity,
}

fn is_function_like(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Function | NodeKind::Method)
}

/// Heuristic: test files don't make good entry points for execution-flow
/// detection. Matches paths whose basename hints at testing.
pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("/test")
        || lower.contains("/tests")
        || lower.contains("__tests__")
        || lower.contains("__mocks__")
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.contains("_test.")
        || lower.contains("_spec.")
}

/// Detect processes. Returns ordered list of traces (longest first).
///
/// `file_paths` must align with `nodes` such that `file_paths[node.file_idx]`
/// resolves to that node's source path (used for test-file exclusion).
pub fn detect_processes(
    nodes: &[Node],
    edges: &[Edge],
    file_paths: &[String],
    config: &ProcessConfig,
) -> Vec<TraceResult> {
    if nodes.is_empty() || edges.is_empty() {
        return Vec::new();
    }

    // Build CALLS adjacency (forward + reverse), filtered by confidence.
    let n = nodes.len();
    let mut calls_fwd: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut calls_rev: Vec<Vec<u32>> = vec![Vec::new(); n];

    for e in edges {
        if e.rel_type != RelType::Calls {
            continue;
        }
        if e.confidence < config.min_confidence {
            continue;
        }
        let s = e.source as usize;
        let t = e.target as usize;
        if s >= n || t >= n || s == t {
            continue;
        }
        calls_fwd[s].push(e.target);
        calls_rev[t].push(e.source);
    }

    // Find entry points.
    let entry_points = find_entry_points(nodes, &calls_fwd, &calls_rev, file_paths, config);
    if entry_points.is_empty() {
        return Vec::new();
    }

    // Trace forward from each entry point.
    let mut all_traces: Vec<Vec<u32>> = Vec::new();
    for &entry in &entry_points {
        if all_traces.len() >= config.max_processes * 2 {
            break;
        }
        let traces = trace_from_entry(entry, &calls_fwd, config);
        for t in traces {
            if t.len() >= config.min_steps {
                all_traces.push(t);
            }
        }
    }

    // Dedup: subset removal (longer trace fully contains shorter).
    let unique = dedup_subsets(all_traces);

    // Dedup: keep longest per (entry, terminal) pair.
    let endpoint_deduped = dedup_by_endpoints(unique);

    // Sort by length descending, cap at max_processes.
    let mut sorted = endpoint_deduped;
    sorted.sort_by_key(|t| std::cmp::Reverse(t.len()));
    sorted.truncate(config.max_processes);

    // Annotate with process_type via community spread.
    sorted
        .into_iter()
        .map(|trace| {
            let mut comms: Vec<u16> = trace
                .iter()
                .map(|&i| nodes[i as usize].community_id)
                .filter(|&c| c != 0)
                .collect();
            comms.sort_unstable();
            comms.dedup();
            let process_type = if comms.len() > 1 {
                ProcessType::CrossCommunity
            } else {
                ProcessType::IntraCommunity
            };
            TraceResult {
                trace,
                process_type,
                communities: comms,
            }
        })
        .collect()
}

/// Entry-point scoring (simplified port of upstream `calculateEntryPointScore`).
/// We score by call ratio and name patterns. Routes/decorators are not exposed
/// in `Node` directly — we approximate via name heuristics.
fn find_entry_points(
    nodes: &[Node],
    fwd: &[Vec<u32>],
    rev: &[Vec<u32>],
    file_paths: &[String],
    config: &ProcessConfig,
) -> Vec<u32> {
    let mut candidates: Vec<(u32, f64)> = Vec::new();

    for (i, node) in nodes.iter().enumerate() {
        if !is_function_like(node.kind) {
            continue;
        }
        let path = file_paths
            .get(node.file_idx as usize)
            .map(|s| s.as_str())
            .unwrap_or("");
        if is_test_path(path) {
            continue;
        }
        let callees = fwd[i].len();
        if callees == 0 {
            continue; // can't trace forward
        }
        let callers = rev[i].len();

        // Call ratio score: many callees, few callers → entry-point-ish.
        // TODO: also consider name patterns (handle*, on*, *Controller) and
        // exported flag once the string pool is plumbed through.
        let score = (callees as f64) / (1.0 + callers as f64);
        if score > 0.0 {
            candidates.push((i as u32, score));
        }
    }

    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    candidates.truncate(config.max_entry_points);
    candidates.into_iter().map(|(idx, _)| idx).collect()
}

/// BFS forward from one entry point, producing distinct paths.
/// Bounded by `max_trace_depth`, `max_branching`, and path-level cycle detection.
fn trace_from_entry(entry: u32, fwd: &[Vec<u32>], config: &ProcessConfig) -> Vec<Vec<u32>> {
    let mut traces: Vec<Vec<u32>> = Vec::new();
    let mut queue: VecDeque<Vec<u32>> = VecDeque::new();
    queue.push_back(vec![entry]);

    let trace_cap = config.max_branching * 3;

    while let Some(path) = queue.pop_front() {
        if traces.len() >= trace_cap {
            break;
        }
        let cur = *path.last().unwrap();
        let callees = &fwd[cur as usize];

        if callees.is_empty() {
            if path.len() >= config.min_steps {
                traces.push(path);
            }
            continue;
        }
        if path.len() >= config.max_trace_depth {
            if path.len() >= config.min_steps {
                traces.push(path);
            }
            continue;
        }

        let mut added = false;
        let path_set: HashSet<u32> = path.iter().copied().collect();
        for &next in callees.iter().take(config.max_branching) {
            if path_set.contains(&next) {
                continue; // cycle guard
            }
            let mut new_path = path.clone();
            new_path.push(next);
            queue.push_back(new_path);
            added = true;
        }
        if !added && path.len() >= config.min_steps {
            // All branches were cycles — terminate here.
            traces.push(path);
        }
    }
    traces
}

/// Remove traces that are subsets of longer traces (sequential containment).
fn dedup_subsets(traces: Vec<Vec<u32>>) -> Vec<Vec<u32>> {
    if traces.is_empty() {
        return traces;
    }
    let mut sorted = traces;
    sorted.sort_by_key(|t| std::cmp::Reverse(t.len()));

    let mut unique: Vec<Vec<u32>> = Vec::new();
    for t in sorted {
        let is_subset = unique.iter().any(|ex| is_subsequence(&t, ex));
        if !is_subset {
            unique.push(t);
        }
    }
    unique
}

/// True if `needle` appears as a contiguous subsequence within `haystack`.
fn is_subsequence(needle: &[u32], haystack: &[u32]) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Keep only the longest trace per unique (entry, terminal) endpoint pair.
fn dedup_by_endpoints(traces: Vec<Vec<u32>>) -> Vec<Vec<u32>> {
    if traces.is_empty() {
        return traces;
    }
    let mut sorted = traces;
    sorted.sort_by_key(|t| std::cmp::Reverse(t.len()));

    let mut by_pair: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for t in sorted {
        let entry = *t.first().unwrap();
        let terminal = *t.last().unwrap();
        by_pair.entry((entry, terminal)).or_insert(t);
    }
    by_pair.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::{StrRef, StringPool};

    fn n(pool: &mut StringPool, name: &str, kind: NodeKind, file_idx: u32) -> Node {
        let r = pool.add(name);
        Node {
            uid: r.clone(),
            name: r,
            file_idx,
            kind,
            span: (0, 0, 0, 0),
            community_id: 1,
        }
    }

    fn e(s: u32, t: u32) -> Edge {
        Edge {
            source: s,
            target: t,
            rel_type: RelType::Calls,
            confidence: 1.0,
            reason: StrRef { offset: 0, len: 0 },
        }
    }

    #[test]
    fn linear_chain_produces_one_trace() {
        // a -> b -> c -> d (4 nodes, single trace)
        let mut pool = StringPool::new();
        let nodes = vec![
            n(&mut pool, "a", NodeKind::Function, 0),
            n(&mut pool, "b", NodeKind::Function, 0),
            n(&mut pool, "c", NodeKind::Function, 0),
            n(&mut pool, "d", NodeKind::Function, 0),
        ];
        let edges = vec![e(0, 1), e(1, 2), e(2, 3)];
        let cfg = ProcessConfig::default();
        let result = detect_processes(&nodes, &edges, &["src/main.rs".to_string()], &cfg);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].trace, vec![0, 1, 2, 3]);
    }

    #[test]
    fn short_chains_below_min_steps_dropped() {
        let mut pool = StringPool::new();
        let nodes = vec![
            n(&mut pool, "a", NodeKind::Function, 0),
            n(&mut pool, "b", NodeKind::Function, 0),
        ];
        let edges = vec![e(0, 1)];
        let result = detect_processes(&nodes, &edges, &["x.rs".into()], &ProcessConfig::default());
        assert!(result.is_empty(), "2-step chain should not qualify");
    }

    #[test]
    fn test_files_excluded_from_entry_points() {
        let mut pool = StringPool::new();
        let nodes = vec![
            n(&mut pool, "test_a", NodeKind::Function, 0), // in test file
            n(&mut pool, "real_a", NodeKind::Function, 1), // in src file
            n(&mut pool, "real_b", NodeKind::Function, 1),
            n(&mut pool, "real_c", NodeKind::Function, 1),
        ];
        let edges = vec![e(0, 1), e(1, 2), e(2, 3)];
        let paths = vec!["src/foo.test.rs".into(), "src/foo.rs".into()];
        let result = detect_processes(&nodes, &edges, &paths, &ProcessConfig::default());
        // Both 0→1→2→3 and 1→2→3 would be candidate traces. Test-file entry
        // excluded, only 1→2→3 survives.
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].trace[0], 1);
    }

    #[test]
    fn cross_community_classification() {
        let mut pool = StringPool::new();
        let mut nodes = vec![
            n(&mut pool, "a", NodeKind::Function, 0),
            n(&mut pool, "b", NodeKind::Function, 0),
            n(&mut pool, "c", NodeKind::Function, 0),
        ];
        nodes[0].community_id = 1;
        nodes[1].community_id = 2; // different community
        nodes[2].community_id = 1;
        let edges = vec![e(0, 1), e(1, 2)];
        let result = detect_processes(
            &nodes,
            &edges,
            &["src/x.rs".into()],
            &ProcessConfig::default(),
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].process_type, ProcessType::CrossCommunity);
    }

    #[test]
    fn low_confidence_edges_filtered() {
        let mut pool = StringPool::new();
        let nodes = vec![
            n(&mut pool, "a", NodeKind::Function, 0),
            n(&mut pool, "b", NodeKind::Function, 0),
            n(&mut pool, "c", NodeKind::Function, 0),
        ];
        let edges = vec![
            Edge {
                source: 0,
                target: 1,
                rel_type: RelType::Calls,
                confidence: 0.3, // below 0.5 threshold
                reason: StrRef { offset: 0, len: 0 },
            },
            e(1, 2),
        ];
        let result = detect_processes(
            &nodes,
            &edges,
            &["src/x.rs".into()],
            &ProcessConfig::default(),
        );
        // 0→1 dropped, so only 1→2 (2 steps, below min 3) → no traces.
        assert!(result.is_empty());
    }
}
