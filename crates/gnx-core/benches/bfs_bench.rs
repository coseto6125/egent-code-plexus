use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fixedbitset::FixedBitSet;
use std::collections::{HashSet, VecDeque};
use std::time::Duration;

// --- Mock Graph Structure for Benchmarking ---
// We simulate the CSR structure without pulling in the full ArchivedZeroCopyGraph
// to isolate the BFS algorithm's pure performance.

pub struct MockGraph {
    pub num_nodes: usize,
    pub out_offsets: Vec<usize>,
    pub out_edges: Vec<u32>, // Just targets for downstream BFS
}

impl MockGraph {
    pub fn new_random_tree(branching_factor: usize, depth: usize) -> Self {
        let mut out_offsets = vec![0];
        let mut out_edges = Vec::new();
        
        // Calculate total nodes roughly: 1 + b + b^2 + ... + b^depth
        let total_nodes = (branching_factor.pow(depth as u32 + 1) - 1) / (branching_factor - 1);
        
        for i in 0..total_nodes {
            let start = out_edges.len();
            // If not at max depth, add children
            if i < (branching_factor.pow(depth as u32) - 1) / (branching_factor - 1) {
                for j in 1..=branching_factor {
                    let child = i * branching_factor + j;
                    if child < total_nodes {
                        out_edges.push(child as u32);
                    }
                }
            }
            out_offsets.push(out_edges.len());
        }
        
        Self {
            num_nodes: total_nodes,
            out_offsets,
            out_edges,
        }
    }
}

// --- Old Implementation (HashSet) ---
fn bfs_hashset(graph: &MockGraph, start_idx: u32, max_depth: usize) -> usize {
    let mut visited: HashSet<u32> = HashSet::new();
    let mut queue: VecDeque<(u32, usize)> = VecDeque::new();
    let mut count = 0;

    queue.push_back((start_idx, 0));
    visited.insert(start_idx);

    while let Some((idx, depth)) = queue.pop_front() {
        count += 1;
        if depth >= max_depth {
            continue;
        }

        let s = graph.out_offsets[idx as usize];
        let e = graph.out_offsets[idx as usize + 1];
        for i in s..e {
            let next = graph.out_edges[i];
            if visited.insert(next) {
                queue.push_back((next, depth + 1));
            }
        }
    }
    count
}

// --- New Implementation (FixedBitSet & pre-alloc) ---
fn bfs_fixedbitset(graph: &MockGraph, start_idx: u32, max_depth: usize) -> usize {
    let mut visited = FixedBitSet::with_capacity(graph.num_nodes);
    let mut queue: VecDeque<(u32, usize)> = VecDeque::with_capacity(1024);
    let mut count = 0;

    queue.push_back((start_idx, 0));
    visited.insert(start_idx as usize);

    while let Some((idx, depth)) = queue.pop_front() {
        count += 1;
        if depth >= max_depth {
            continue;
        }

        let s = graph.out_offsets[idx as usize];
        let e = graph.out_offsets[idx as usize + 1];
        let edges_slice = &graph.out_edges[s..e];
        for &next in edges_slice {
            if !visited.contains(next as usize) {
                visited.insert(next as usize);
                queue.push_back((next, depth + 1));
            }
        }
    }
    count
}

fn criterion_benchmark(c: &mut Criterion) {
    // Create a large tree: branching factor 4, depth 8 => ~87,381 nodes
    let graph = MockGraph::new_random_tree(4, 8);
    
    let mut group = c.benchmark_group("BFS Traversal");
    group.measurement_time(Duration::from_secs(5));

    group.bench_function("HashSet (Old)", |b| {
        b.iter(|| bfs_hashset(black_box(&graph), black_box(0), black_box(8)))
    });

    group.bench_function("FixedBitSet (New)", |b| {
        b.iter(|| bfs_fixedbitset(black_box(&graph), black_box(0), black_box(8)))
    });

    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
