//! Louvain community detection on the in-memory `Node`/`Edge` slices.
//!
//! Upstream uses Leiden (a refinement of Louvain) via a vendored JS library.
//! We use Louvain here because it can be implemented in pure Rust with no
//! external crate, and gives equivalent-quality communities for our use case
//! (75-300 communities on 1K-25K symbols). Leiden's main advantage is fixing
//! "badly-connected communities" — a rare pathological case that does not
//! affect detect_changes risk evaluation.
//!
//! Filters mirror upstream `buildGraphologyGraph`:
//!   - Only `NodeKind::{Function, Class, Method, Interface}` participate
//!   - Edges restricted to `RelType::{Calls, Extends, Implements}`
//!   - For large graphs (>10K symbols), drop edges with confidence < 0.5
//!     and degree-1 nodes
//!
//! Output: `Vec<u16>` of length `nodes.len()`. Unassigned nodes (e.g. Files,
//! Imports, Routes, isolates) get 0.

use crate::graph::{Edge, Node, NodeKind, RelType};
use rustc_hash::FxHashMap;

#[derive(Debug, Clone)]
pub struct LouvainConfig {
    pub max_iterations: usize,
    pub max_passes: usize,
    pub min_modularity_gain: f64,
    pub large_graph_threshold: usize,
    pub min_confidence_large: f32,
    pub seed: u64,
}

impl Default for LouvainConfig {
    fn default() -> Self {
        Self {
            max_iterations: 64,
            max_passes: 5,
            min_modularity_gain: 1e-6,
            large_graph_threshold: 10_000,
            min_confidence_large: 0.5,
            seed: 0xc0de,
        }
    }
}

fn is_symbol(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function | NodeKind::Class | NodeKind::Method | NodeKind::Interface
    )
}

fn is_clustering_rel(rel: RelType) -> bool {
    matches!(rel, RelType::Calls | RelType::Extends | RelType::Implements)
}

/// xorshift64 — small deterministic RNG, no external crate.
struct XorShift64(u64);
impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9E3779B97F4A7C15 } else { seed })
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn shuffle<T>(&mut self, slice: &mut [T]) {
        let n = slice.len();
        for i in (1..n).rev() {
            let j = (self.next() as usize) % (i + 1);
            slice.swap(i, j);
        }
    }
}

/// Detect communities. Returns `Vec<u16>` aligned to `nodes`, where 0 = unassigned.
/// Communities are renumbered 1..=N, capped at `u16::MAX`.
pub fn detect_communities(nodes: &[Node], edges: &[Edge], config: &LouvainConfig) -> Vec<u16> {
    let n = nodes.len();
    let mut assignments = vec![0u16; n];
    if n == 0 || edges.is_empty() {
        return assignments;
    }

    // Decide large-graph mode by symbol count.
    let symbol_count = nodes.iter().filter(|nd| is_symbol(nd.kind)).count();
    let is_large = symbol_count > config.large_graph_threshold;
    let min_conf = if is_large {
        config.min_confidence_large
    } else {
        0.0
    };

    // Build undirected weighted adjacency over symbol nodes. We use the
    // original `node_idx` as the dense vertex id and skip non-symbol indices.
    let mut adj: Vec<Vec<(u32, f64)>> = vec![Vec::new(); n];
    let mut connected = vec![false; n];

    for e in edges {
        if !is_clustering_rel(e.rel_type) {
            continue;
        }
        if e.confidence < min_conf {
            continue;
        }
        let s = e.source as usize;
        let t = e.target as usize;
        if s == t || s >= n || t >= n {
            continue;
        }
        if !is_symbol(nodes[s].kind) || !is_symbol(nodes[t].kind) {
            continue;
        }
        let w = e.confidence as f64;
        adj[s].push((e.target, w));
        adj[t].push((e.source, w));
        connected[s] = true;
        connected[t] = true;
    }

    // Large-graph mode: drop degree-1 vertices entirely.
    if is_large {
        for i in 0..n {
            if adj[i].len() < 2 {
                connected[i] = false;
                adj[i].clear();
            }
        }
        // Re-drop edges whose endpoint was just dropped.
        for adj_i in adj.iter_mut() {
            adj_i.retain(|&(t, _)| connected[t as usize]);
        }
    }

    let active_nodes: Vec<u32> = (0..n as u32).filter(|&i| connected[i as usize]).collect();
    if active_nodes.is_empty() {
        return assignments;
    }

    // Degree (k_i) and total edge weight (m). For undirected graph 2m = sum of degrees.
    let mut k: Vec<f64> = vec![0.0; n];
    for i in &active_nodes {
        let i = *i as usize;
        k[i] = adj[i].iter().map(|(_, w)| w).sum();
    }
    let two_m: f64 = k.iter().sum();
    if two_m <= 0.0 {
        return assignments;
    }

    // Louvain main loop.
    // `community[i]` = current community of node i (use node_idx as id space).
    let mut community: Vec<u32> = (0..n as u32).collect();
    let mut sigma_tot: FxHashMap<u32, f64> = FxHashMap::default();
    for &i in &active_nodes {
        sigma_tot.insert(i, k[i as usize]);
    }

    let mut rng = XorShift64::new(config.seed);

    // Single-level Louvain: one outer pass refines node→community assignments
    // until no node moves (`iter_improved == false`) or `max_iterations` cap.
    // For multi-level we'd rebuild `adj` from the contracted community graph
    // and recurse; single-level already captures dominant structure for our
    // scale (≤ 25K symbols). `config.max_passes` is reserved for that future
    // expansion and currently unused.
    let mut order = active_nodes.clone();
    for _iter in 0..config.max_iterations {
        rng.shuffle(&mut order);
        let mut iter_improved = false;

        for &i in &order {
            let ci = community[i as usize];
            let ki = k[i as usize];

            // Sum of weights from i to each neighbor community (excluding self-loops).
            let mut k_i_to: FxHashMap<u32, f64> = FxHashMap::default();
            for &(j, w) in &adj[i as usize] {
                if j == i {
                    continue;
                }
                let cj = community[j as usize];
                *k_i_to.entry(cj).or_insert(0.0) += w;
            }

            // Remove i from its current community.
            if let Some(s) = sigma_tot.get_mut(&ci) {
                *s -= ki;
            }

            // Find best community.
            let mut best_c = ci;
            let mut best_gain = 0.0_f64;

            let k_i_ci = *k_i_to.get(&ci).unwrap_or(&0.0);
            let sigma_ci = *sigma_tot.get(&ci).unwrap_or(&0.0);
            // ΔQ if we stay = k_i,ci / m - ki * sigma_tot_ci / (2m^2)
            let stay_gain = k_i_ci / (two_m / 2.0) - ki * sigma_ci / (two_m * two_m / 2.0);

            for (&cand, &k_i_c) in &k_i_to {
                if cand == ci {
                    continue;
                }
                let sigma_c = *sigma_tot.get(&cand).unwrap_or(&0.0);
                let gain = k_i_c / (two_m / 2.0) - ki * sigma_c / (two_m * two_m / 2.0);
                if gain > best_gain + config.min_modularity_gain && gain > stay_gain {
                    best_gain = gain;
                    best_c = cand;
                }
            }

            // Commit move.
            community[i as usize] = best_c;
            *sigma_tot.entry(best_c).or_insert(0.0) += ki;

            if best_c != ci {
                iter_improved = true;
            }
        }

        if !iter_improved {
            break;
        }
    }

    // Renumber communities densely starting at 1, then fold into u16.
    let mut remap: FxHashMap<u32, u16> = FxHashMap::default();
    let mut next_id: u32 = 1;
    for &i in &active_nodes {
        let c = community[i as usize];
        let id = *remap.entry(c).or_insert_with(|| {
            let id = next_id;
            next_id += 1;
            id.min(u16::MAX as u32) as u16
        });
        assignments[i as usize] = id;
    }

    assignments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::{StrRef, StringPool};

    fn n(pool: &mut StringPool, name: &str, kind: NodeKind) -> Node {
        let r = pool.add(name);
        Node {
            uid: crate::uid::compute(kind, "", None, name),
            name: r,
            file_idx: 0,
            kind,
            span: (0, 0, 0, 0),
            community_id: 0,
            owner_class: StrRef::default(),
        }
    }

    fn e(s: u32, t: u32, rel: RelType) -> Edge {
        Edge {
            source: s,
            target: t,
            rel_type: rel,
            confidence: 1.0,
            reason: StrRef { offset: 0, len: 0 },
        }
    }

    #[test]
    fn empty_graph_returns_empty() {
        let cfg = LouvainConfig::default();
        let r = detect_communities(&[], &[], &cfg);
        assert!(r.is_empty());
    }

    #[test]
    fn two_disconnected_cliques_form_two_communities() {
        let mut pool = StringPool::new();
        let nodes = vec![
            n(&mut pool, "a", NodeKind::Function),
            n(&mut pool, "b", NodeKind::Function),
            n(&mut pool, "c", NodeKind::Function),
            n(&mut pool, "d", NodeKind::Function),
            n(&mut pool, "e", NodeKind::Function),
            n(&mut pool, "f", NodeKind::Function),
        ];
        // Clique 1: 0-1-2, Clique 2: 3-4-5
        let edges = vec![
            e(0, 1, RelType::Calls),
            e(1, 2, RelType::Calls),
            e(0, 2, RelType::Calls),
            e(3, 4, RelType::Calls),
            e(4, 5, RelType::Calls),
            e(3, 5, RelType::Calls),
        ];
        let cfg = LouvainConfig::default();
        let assignments = detect_communities(&nodes, &edges, &cfg);

        // Both cliques should land in distinct communities, all members same id.
        assert_eq!(assignments[0], assignments[1]);
        assert_eq!(assignments[1], assignments[2]);
        assert_eq!(assignments[3], assignments[4]);
        assert_eq!(assignments[4], assignments[5]);
        assert_ne!(assignments[0], assignments[3]);
        assert_ne!(assignments[0], 0); // not unassigned
    }

    #[test]
    fn non_symbol_nodes_remain_unassigned() {
        let mut pool = StringPool::new();
        let nodes = vec![
            n(&mut pool, "f", NodeKind::Function),
            n(&mut pool, "g", NodeKind::Function),
            n(&mut pool, "v", NodeKind::Variable), // not symbol
        ];
        let edges = vec![
            e(0, 1, RelType::Calls),
            e(0, 2, RelType::Calls), // crosses symbol boundary, dropped
        ];
        let cfg = LouvainConfig::default();
        let a = detect_communities(&nodes, &edges, &cfg);
        assert_eq!(a[2], 0, "non-symbol node should stay unassigned");
        assert_ne!(a[0], 0);
    }

    #[test]
    fn non_clustering_edges_are_ignored() {
        let mut pool = StringPool::new();
        let nodes = vec![
            n(&mut pool, "a", NodeKind::Function),
            n(&mut pool, "b", NodeKind::Function),
        ];
        let edges = vec![e(0, 1, RelType::Imports)]; // not in clustering set
        let cfg = LouvainConfig::default();
        let a = detect_communities(&nodes, &edges, &cfg);
        assert_eq!(a[0], 0);
        assert_eq!(a[1], 0);
    }
}
