//! Query helpers over `ArchivedZeroCopyGraph` (mmap-backed read view).
//!
//! These wrappers centralise patterns that previously lived inline in CLI
//! commands (e.g. BFS upstream/downstream, file-path lookups). Keeping them
//! here lets `detect_changes`, `impact`, and future commands share one
//! implementation.

use crate::graph::{ArchivedNode, ArchivedZeroCopyGraph};
use std::collections::VecDeque;
use fixedbitset::FixedBitSet;

/// Find the file_idx whose stored path ends with `relative_path`.
///
/// Returns the first match. The match direction is strict suffix: stored path
/// must end with the input. The reverse direction (input ends with stored)
/// was intentionally dropped — it caused false positives when a short stored
/// path like `lib.rs` matched a diff path like `auth/lib.rs`.
pub fn file_idx_by_suffix(graph: &ArchivedZeroCopyGraph, relative_path: &str) -> Option<u32> {
    for (i, file) in graph.files.iter().enumerate() {
        let path = file.path.resolve(&graph.string_pool);
        if path.ends_with(relative_path) {
            return Some(i as u32);
        }
    }
    None
}

/// Return `(node_idx, &ArchivedNode)` for every node in `file_idx` whose
/// span overlaps the inclusive line range `[start_line, end_line]`.
///
/// Span semantics: `node.span = (start_line, start_col, end_line, end_col)`.
/// Overlap test: `node.start_line <= end_line && node.end_line >= start_line`.
pub fn nodes_overlapping_lines(
    graph: &ArchivedZeroCopyGraph,
    file_idx: u32,
    start_line: u32,
    end_line: u32,
) -> Vec<(u32, &ArchivedNode)> {
    let mut out = Vec::new();
    for (i, node) in graph.nodes.iter().enumerate() {
        if node.file_idx.to_native() != file_idx {
            continue;
        }
        let ns = node.span.0.to_native();
        let ne = node.span.2.to_native();
        if ns <= end_line && ne >= start_line {
            out.push((i as u32, node));
        }
    }
    out
}

/// BFS upstream (callers) from `start_idx` up to `max_depth`.
/// Returns visited indices including the start, paired with depth (0 = start).

pub fn callers_of(
    graph: &ArchivedZeroCopyGraph,
    start_idx: u32,
    max_depth: usize,
) -> Vec<(u32, usize)> {
    let config = BfsConfig {
        max_depth,
        ..Default::default()
    };
    bfs(graph, start_idx, &config, Direction::Upstream)
}


/// BFS downstream (callees) from `start_idx`.

pub fn callees_of(
    graph: &ArchivedZeroCopyGraph,
    start_idx: u32,
    max_depth: usize,
) -> Vec<(u32, usize)> {
    let config = BfsConfig {
        max_depth,
        ..Default::default()
    };
    bfs(graph, start_idx, &config, Direction::Downstream)
}


enum Direction {
    Upstream,
    Downstream,
}


pub struct BfsConfig<'a> {
    pub max_depth: usize,
    pub max_nodes: usize,
    /// If Some, only traverse edges of these types
    pub allowed_rel_types: Option<&'a [crate::graph::RelType]>,
}

impl Default for BfsConfig<'_> {
    fn default() -> Self {
        Self {
            max_depth: usize::MAX,
            max_nodes: 10_000,
            allowed_rel_types: None,
        }
    }
}

fn bfs(
    graph: &ArchivedZeroCopyGraph,
    start_idx: u32,
    config: &BfsConfig,
    dir: Direction,
) -> Vec<(u32, usize)> {
    let num_nodes = graph.nodes.len();
    let mut visited = FixedBitSet::with_capacity(num_nodes);
    let mut queue: VecDeque<(u32, usize)> = VecDeque::with_capacity(1024);
    let mut out: Vec<(u32, usize)> = Vec::with_capacity(1024);

    if start_idx as usize >= num_nodes {
        return out;
    }

    queue.push_back((start_idx, 0));
    visited.insert(start_idx as usize);

    while let Some((idx, depth)) = queue.pop_front() {
        out.push((idx, depth));
        
        if out.len() >= config.max_nodes {
            break;
        }

        if depth >= config.max_depth {
            continue;
        }

        match dir {
            Direction::Upstream => {
                let s = graph.in_offsets[idx as usize].to_native() as usize;
                let e = graph.in_offsets[idx as usize + 1].to_native() as usize;
                for i in s..e {
                    let edge_idx = graph.in_edge_idx[i].to_native() as usize;
                    let edge = &graph.edges[edge_idx];
                    
                    if let Some(allowed) = config.allowed_rel_types {
                        let rel: crate::graph::RelType = rkyv::deserialize::<crate::graph::RelType, rkyv::rancor::Error>(&edge.rel_type).unwrap();
                        if !allowed.contains(&rel) {
                            continue;
                        }
                    }

                    let next = edge.source.to_native();
                    if !visited.contains(next as usize) {
                        visited.insert(next as usize);
                        queue.push_back((next, depth + 1));
                    }
                }
            }
            Direction::Downstream => {
                let s = graph.out_offsets[idx as usize].to_native() as usize;
                let e = graph.out_offsets[idx as usize + 1].to_native() as usize;
                let edges_slice = &graph.edges[s..e];
                for edge in edges_slice {
                    if let Some(allowed) = config.allowed_rel_types {
                        let rel: crate::graph::RelType = rkyv::deserialize::<crate::graph::RelType, rkyv::rancor::Error>(&edge.rel_type).unwrap();
                        if !allowed.contains(&rel) {
                            continue;
                        }
                    }

                    let next = edge.target.to_native();
                    if !visited.contains(next as usize) {
                        visited.insert(next as usize);
                        queue.push_back((next, depth + 1));
                    }
                }
            }
        }
    }
    out
}


/// Given a `node_idx`, return the indices of Process nodes whose trace
/// contains it, along with the 1-indexed step position.
///
/// Process nodes live at `nodes[process_start..]`. The trace for process k
/// (where `node_idx = process_start + k`) is `traces_data[off[k]..off[k+1]]`.
pub fn processes_containing(graph: &ArchivedZeroCopyGraph, node_idx: u32) -> Vec<(u32, u32)> {
    let process_start = graph.process_start.to_native();
    let n_processes = graph.traces_offsets.len().saturating_sub(1);
    let mut out = Vec::new();
    for k in 0..n_processes {
        let off_start = graph.traces_offsets[k].to_native() as usize;
        let off_end = graph.traces_offsets[k + 1].to_native() as usize;
        for (step_idx, raw) in graph.traces_data[off_start..off_end].iter().enumerate() {
            if raw.to_native() == node_idx {
                let process_node_idx = process_start + k as u32;
                out.push((process_node_idx, (step_idx + 1) as u32));
                break; // one trace appearance per process is enough
            }
        }
    }
    out
}
