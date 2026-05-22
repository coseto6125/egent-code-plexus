//! Leiden community detection — full multi-level implementation.
//!
//! From Traag, Waltman, van Eck (2019) *From Louvain to Leiden: guaranteeing
//! well-connected communities*. The key improvement over [`super::louvain`] is
//! the refinement phase, which prevents badly-connected hubs from getting
//! trapped in suboptimal communities — the failure mode that causes our
//! `process_type` labels to diverge from upstream gitnexus on small graphs.
//!
//! Three phases per pass:
//!   1. Local moving — Louvain-style greedy modularity ascent
//!   2. Refinement — split each community into well-connected sub-communities
//!   3. Aggregation — refined sub-communities become super-nodes; recurse
//!
//! Edge filters mirror upstream `buildGraphologyGraph` exactly:
//!   - Only `NodeKind::{Function, Class, Method, Interface}` participate
//!   - Edges restricted to `RelType::{Calls, Extends, Implements}`
//!   - Large graphs (> threshold): drop edges with `confidence < min_confidence_large`
//!   - Large graphs: drop degree-1 nodes (singletons just add iteration cost)

use crate::graph::{Edge, Node, NodeKind, RelType};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// ECP_PROF instrumentation: pass count + per-phase wall (experiments C+D).
/// Only populated when ECP_PROF is set; printed at end of `detect_communities`.
#[derive(Default)]
struct LeidenProf {
    passes_run: usize,
    local_move_total: Duration,
    refine_total: Duration,
    aggregate_total: Duration,
}

#[derive(Debug, Clone)]
pub struct LeidenConfig {
    pub max_iterations: usize,
    pub max_passes: usize,
    pub min_modularity_gain: f64,
    pub large_graph_threshold: usize,
    pub min_confidence_large: f32,
    pub seed: u64,
}

impl Default for LeidenConfig {
    fn default() -> Self {
        Self {
            max_iterations: 64,
            // Empirical convergence point on the 14-lang parity corpus is
            // pass 5-6; passes 4-5 add ~0.0003 Q for ~50ms work. Capping at 3
            // gives bit-identical downstream Process metrics (12/12 multi-seed
            // × multi-corpus configs) while saving 24% pass3 wall.
            max_passes: 3,
            min_modularity_gain: 1e-7,
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

/// Deterministic RNG (xorshift64). Seeded so runs are reproducible.
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
pub fn detect_communities(nodes: &[Node], edges: &[Edge], config: &LeidenConfig) -> Vec<u16> {
    let n = nodes.len();
    let mut assignments = vec![0u16; n];
    if n == 0 || edges.is_empty() {
        return assignments;
    }

    // Decide large-graph mode.
    let symbol_count = nodes.iter().filter(|nd| is_symbol(nd.kind)).count();
    let is_large = symbol_count > config.large_graph_threshold;
    let min_conf = if is_large {
        config.min_confidence_large
    } else {
        0.0
    };

    // Build undirected weighted adjacency.
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

    if is_large {
        for i in 0..n {
            if adj[i].len() < 2 {
                connected[i] = false;
                adj[i].clear();
            }
        }
        for adj_i in adj.iter_mut() {
            adj_i.retain(|&(t, _)| connected[t as usize]);
        }
    }

    let degree: Vec<f64> = adj
        .iter()
        .map(|nbrs| nbrs.iter().map(|(_, w)| *w).sum())
        .collect();
    let two_m: f64 = degree.iter().sum();
    if two_m <= 0.0 {
        return assignments;
    }

    // Singleton initial partition.
    let mut community: Vec<u32> = (0..n as u32).collect();

    let prof_enabled = std::env::var_os("ECP_PROF").is_some();
    let mut prof = LeidenProf::default();

    // Multi-level Leiden.
    leiden_recursive(&adj, &degree, two_m, &mut community, config, 0, &mut prof);

    // Renumber active communities densely to u16.
    let mut remap: FxHashMap<u32, u16> = FxHashMap::default();
    let mut next_id: u32 = 1;
    for i in 0..n {
        if !connected[i] {
            continue;
        }
        let c = community[i];
        let id = *remap.entry(c).or_insert_with(|| {
            let id = next_id;
            next_id += 1;
            id.min(u16::MAX as u32) as u16
        });
        assignments[i] = id;
    }

    if prof_enabled {
        let q = compute_modularity(&adj, &degree, two_m, &community, &connected);
        let n_connected = connected.iter().filter(|c| **c).count();
        let edges_kept = adj.iter().map(|a| a.len()).sum::<usize>() / 2;
        eprintln!(
            "prof build.pass3_community.quality: modularity={q:.5} communities={} connected_nodes={n_connected} edges_kept={edges_kept}",
            remap.len(),
        );
        eprintln!(
            "prof build.pass3_community.phases: passes_run={} local_move={:.4}s refine={:.4}s aggregate={:.4}s",
            prof.passes_run,
            prof.local_move_total.as_secs_f32(),
            prof.refine_total.as_secs_f32(),
            prof.aggregate_total.as_secs_f32(),
        );
    }
    assignments
}

fn compute_modularity(
    adj: &[Vec<(u32, f64)>],
    degree: &[f64],
    two_m: f64,
    community: &[u32],
    connected: &[bool],
) -> f64 {
    if two_m <= 0.0 {
        return 0.0;
    }
    let m = two_m / 2.0;
    let mut comm_internal: FxHashMap<u32, f64> = FxHashMap::default();
    let mut comm_total_deg: FxHashMap<u32, f64> = FxHashMap::default();
    for i in 0..adj.len() {
        if !connected[i] {
            continue;
        }
        let ci = community[i];
        *comm_total_deg.entry(ci).or_insert(0.0) += degree[i];
        for &(j, w) in &adj[i] {
            if community[j as usize] == ci {
                *comm_internal.entry(ci).or_insert(0.0) += w;
            }
        }
    }
    let mut q = 0.0;
    for (c, &deg_c) in &comm_total_deg {
        let e_c = comm_internal.get(c).copied().unwrap_or(0.0) / 2.0;
        q += e_c / m - (deg_c / two_m).powi(2);
    }
    q
}

fn leiden_recursive(
    adj: &[Vec<(u32, f64)>],
    degree: &[f64],
    two_m: f64,
    community: &mut [u32],
    config: &LeidenConfig,
    depth: usize,
    prof: &mut LeidenProf,
) {
    if depth >= config.max_passes {
        return;
    }
    let n = adj.len();
    prof.passes_run = prof.passes_run.max(depth + 1);

    // Phase 1: local moving.
    let t = Instant::now();
    let moved = local_move(adj, degree, two_m, community, config);
    prof.local_move_total += t.elapsed();
    if !moved && depth > 0 {
        return;
    }

    // Phase 2: refinement — split each community into well-connected pieces.
    // Dispatch: parallel refine pays its rayon overhead only when the
    // connected subgraph at this recursion level exceeds the large-graph
    // threshold. Measured: parallel is 1.5× faster on 57k-symbol corpora,
    // 4× slower on 6k-symbol corpora — the threshold gates that cliff.
    let t = Instant::now();
    let connected_count = adj.iter().filter(|a| !a.is_empty()).count();
    let refined = if connected_count > config.large_graph_threshold {
        refine_parallel(adj, degree, two_m, community, config)
    } else {
        refine(adj, degree, two_m, community, config)
    };
    prof.refine_total += t.elapsed();

    // Renumber refined → dense 0..M.
    let mut refined_id_map: FxHashMap<u32, u32> = FxHashMap::default();
    let mut next_refined: u32 = 0;
    for &r in &refined {
        refined_id_map.entry(r).or_insert_with(|| {
            let v = next_refined;
            next_refined += 1;
            v
        });
    }
    let m_super = next_refined as usize;

    // Fixed point check: if refinement produced no new groups, stop.
    let mut community_id_count: FxHashMap<u32, u32> = FxHashMap::default();
    for &c in community.iter() {
        *community_id_count.entry(c).or_insert(0) += 1;
    }
    if m_super >= n || m_super == community_id_count.len() {
        return;
    }

    // Phase 3: aggregation.
    let t = Instant::now();
    let (super_adj, super_degree) = aggregate(adj, degree, &refined, &refined_id_map);
    prof.aggregate_total += t.elapsed();
    let super_two_m: f64 = super_degree.iter().sum();
    if super_two_m <= 0.0 {
        return;
    }

    // Initial super-community: each refined sub-community inherits its parent's label.
    let mut super_community: Vec<u32> = vec![0; m_super];
    for (orig_i, &refined_id) in refined.iter().enumerate() {
        let super_idx = refined_id_map[&refined_id] as usize;
        super_community[super_idx] = community[orig_i];
    }

    // Recurse on the aggregated graph.
    leiden_recursive(
        &super_adj,
        &super_degree,
        super_two_m,
        &mut super_community,
        config,
        depth + 1,
        prof,
    );

    // Lift super-community labels back to original nodes via refined.
    for i in 0..n {
        let super_idx = refined_id_map[&refined[i]] as usize;
        community[i] = super_community[super_idx];
    }
}

/// Standard Louvain-style local moving: repeatedly relocate nodes to maximize ΔQ.
/// Returns true if any node moved.
fn local_move(
    adj: &[Vec<(u32, f64)>],
    degree: &[f64],
    two_m: f64,
    community: &mut [u32],
    config: &LeidenConfig,
) -> bool {
    let n = adj.len();
    // Dense sigma_tot indexed by community_id. At recursive levels the
    // super-community labels are inherited from the parent level, so the
    // max community id can exceed adj.len(); size the buffer by the
    // actual max we'll see (+1) to keep all accesses in-bounds while
    // staying tight on memory for the typical small-graph case.
    let max_cid = community.iter().copied().max().unwrap_or(0) as usize;
    let dense_len = max_cid + 1;
    let mut sigma_tot: Vec<f64> = vec![0.0; dense_len];
    for i in 0..n {
        sigma_tot[community[i] as usize] += degree[i];
    }

    // Sparse-set scratch for k_i,C: a dense f64 buffer plus a list of
    // touched community ids so we can O(neighbors) reset between nodes
    // without zeroing the whole buffer. Lives across all iterations.
    let mut k_i_to_dense: Vec<f64> = vec![0.0; dense_len];
    let mut k_i_to_touched: Vec<u32> = Vec::new();

    let mut overall_moved = false;
    let mut rng = XorShift64::new(config.seed);
    let mut order: Vec<usize> = (0..n).collect();

    for _iter in 0..config.max_iterations {
        rng.shuffle(&mut order);
        let mut iter_moved = false;

        for &i in &order {
            if adj[i].is_empty() {
                continue;
            }
            let ci = community[i];
            let ki = degree[i];

            // k_i,C accumulation via dense buffer + touched-set.
            for &(j, w) in &adj[i] {
                if j as usize == i {
                    continue;
                }
                let cj = community[j as usize];
                let slot = &mut k_i_to_dense[cj as usize];
                if *slot == 0.0 {
                    k_i_to_touched.push(cj);
                }
                *slot += w;
            }

            // Pull i out of its current community.
            sigma_tot[ci as usize] -= ki;

            // Stay-gain: ΔQ if we put i back in ci (= baseline to beat).
            let k_i_ci = k_i_to_dense[ci as usize];
            let sigma_ci = sigma_tot[ci as usize];
            let stay_gain = k_i_ci / (two_m / 2.0) - ki * sigma_ci / (two_m * two_m / 2.0);

            let mut best_c = ci;
            let mut best_gain = stay_gain;
            for &cand in &k_i_to_touched {
                if cand == ci {
                    continue;
                }
                let k_i_c = k_i_to_dense[cand as usize];
                let sigma_c = sigma_tot[cand as usize];
                let gain = k_i_c / (two_m / 2.0) - ki * sigma_c / (two_m * two_m / 2.0);
                if gain > best_gain + config.min_modularity_gain {
                    best_gain = gain;
                    best_c = cand;
                }
            }

            community[i] = best_c;
            sigma_tot[best_c as usize] += ki;
            if best_c != ci {
                iter_moved = true;
                overall_moved = true;
            }

            // O(touched) reset — preserves the dense buffer's "all zeros"
            // invariant for the next node without zeroing the entire Vec.
            for &cand in &k_i_to_touched {
                k_i_to_dense[cand as usize] = 0.0;
            }
            k_i_to_touched.clear();
        }

        if !iter_moved {
            break;
        }
    }
    overall_moved
}

/// Refinement phase: for each community in `partition`, run a fresh local-move
/// restricted to that community's members. Starts from singletons (each node
/// alone). Result: a finer partition where every refined community is locally
/// modularity-optimal within its parent — i.e. well-connected.
fn refine(
    adj: &[Vec<(u32, f64)>],
    degree: &[f64],
    two_m: f64,
    partition: &[u32],
    config: &LeidenConfig,
) -> Vec<u32> {
    let n = adj.len();
    let mut refined: Vec<u32> = (0..n as u32).collect();

    // Group nodes by current partition.
    let mut groups: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
    for (i, &c) in partition.iter().enumerate() {
        groups.entry(c).or_default().push(i as u32);
    }
    let mut group_keys: Vec<u32> = groups.keys().copied().collect();
    group_keys.sort();

    let mut rng = XorShift64::new(config.seed.wrapping_add(0xdead_beef));

    // Shared dense-buffers across groups — sized once to n since refined
    // ids are always in 0..n (singleton init, merges only). `is_member`
    // doubles as the members_set check, reset O(group_size) between
    // groups via the membership list itself.
    let mut sigma_tot: Vec<f64> = vec![0.0; n];
    let mut k_i_to_dense: Vec<f64> = vec![0.0; n];
    let mut k_i_to_touched: Vec<u32> = Vec::new();
    let mut is_member: Vec<bool> = vec![false; n];

    for c in group_keys {
        let members = groups.remove(&c).unwrap();
        if members.len() <= 1 {
            continue;
        }

        // Mark membership + seed sigma_tot for this group's refined sub-communities.
        for &m in &members {
            is_member[m as usize] = true;
            sigma_tot[refined[m as usize] as usize] += degree[m as usize];
        }

        let mut order: Vec<u32> = members.clone();
        for _iter in 0..config.max_iterations {
            rng.shuffle(&mut order);
            let mut iter_moved = false;

            for &i in &order {
                let ci_r = refined[i as usize];
                let ki = degree[i as usize];

                // k_i,C accumulation via dense buffer + touched-set.
                for &(j, w) in &adj[i as usize] {
                    if !is_member[j as usize] {
                        continue;
                    }
                    let cj_r = refined[j as usize];
                    let slot = &mut k_i_to_dense[cj_r as usize];
                    if *slot == 0.0 {
                        k_i_to_touched.push(cj_r);
                    }
                    *slot += w;
                }

                sigma_tot[ci_r as usize] -= ki;

                let k_i_ci = k_i_to_dense[ci_r as usize];
                let sigma_ci = sigma_tot[ci_r as usize];
                let stay_gain = k_i_ci / (two_m / 2.0) - ki * sigma_ci / (two_m * two_m / 2.0);

                let mut best_c = ci_r;
                let mut best_gain = stay_gain;
                for &cand in &k_i_to_touched {
                    if cand == ci_r {
                        continue;
                    }
                    let k_i_c = k_i_to_dense[cand as usize];
                    let sigma_c = sigma_tot[cand as usize];
                    let gain = k_i_c / (two_m / 2.0) - ki * sigma_c / (two_m * two_m / 2.0);
                    if gain > best_gain + config.min_modularity_gain {
                        best_gain = gain;
                        best_c = cand;
                    }
                }

                refined[i as usize] = best_c;
                sigma_tot[best_c as usize] += ki;
                if best_c != ci_r {
                    iter_moved = true;
                }

                // O(touched) reset of k_i_to dense buffer.
                for &cand in &k_i_to_touched {
                    k_i_to_dense[cand as usize] = 0.0;
                }
                k_i_to_touched.clear();
            }
            if !iter_moved {
                break;
            }
        }

        // O(members) reset of sigma_tot + is_member for the next group.
        for &m in &members {
            sigma_tot[refined[m as usize] as usize] = 0.0;
            is_member[m as usize] = false;
        }
    }
    refined
}

/// Parallel refine (spike) — same semantics as `refine()` but runs each
/// group on a rayon thread. Per-task local FxHashMap buffers; cross-group
/// `refined[]` writes never overlap because the `is_member` guard skips
/// cross-group edges, so disjoint atomic stores are race-free. Determinism
/// is preserved by sorting groups by community_id and seeding the per-task
/// RNG with `seed ^ ordinal`.
fn refine_parallel(
    adj: &[Vec<(u32, f64)>],
    degree: &[f64],
    two_m: f64,
    partition: &[u32],
    config: &LeidenConfig,
) -> Vec<u32> {
    let n = adj.len();
    let refined: Vec<AtomicU32> = (0..n).map(|i| AtomicU32::new(i as u32)).collect();

    let mut groups: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
    for (i, &c) in partition.iter().enumerate() {
        groups.entry(c).or_default().push(i as u32);
    }
    // Filter size>=2 + sort by community_id for deterministic ordinal.
    let mut work: Vec<(u32, Vec<u32>)> = groups.into_iter().filter(|(_, m)| m.len() >= 2).collect();
    work.sort_by_key(|(k, _)| *k);

    let base_seed = config.seed.wrapping_add(0xdead_beef);

    work.par_iter()
        .enumerate()
        .for_each(|(idx, (_community_id, members))| {
            let mut rng = XorShift64::new(base_seed ^ idx as u64);
            let mut sigma_tot: FxHashMap<u32, f64> = FxHashMap::default();
            let mut is_member: FxHashSet<u32> = FxHashSet::default();
            let mut k_i_to: FxHashMap<u32, f64> = FxHashMap::default();
            let mut touched: Vec<u32> = Vec::new();

            for &m in members {
                is_member.insert(m);
                let r = refined[m as usize].load(Ordering::Relaxed);
                *sigma_tot.entry(r).or_insert(0.0) += degree[m as usize];
            }

            let mut order: Vec<u32> = members.clone();
            for _iter in 0..config.max_iterations {
                rng.shuffle(&mut order);
                let mut iter_moved = false;

                for &i in &order {
                    let ci_r = refined[i as usize].load(Ordering::Relaxed);
                    let ki = degree[i as usize];

                    for &(j, w) in &adj[i as usize] {
                        if !is_member.contains(&j) {
                            continue;
                        }
                        let cj_r = refined[j as usize].load(Ordering::Relaxed);
                        let slot = k_i_to.entry(cj_r).or_insert(0.0);
                        if *slot == 0.0 {
                            touched.push(cj_r);
                        }
                        *slot += w;
                    }

                    *sigma_tot.entry(ci_r).or_insert(0.0) -= ki;

                    let k_i_ci = k_i_to.get(&ci_r).copied().unwrap_or(0.0);
                    let sigma_ci = sigma_tot.get(&ci_r).copied().unwrap_or(0.0);
                    let stay_gain = k_i_ci / (two_m / 2.0) - ki * sigma_ci / (two_m * two_m / 2.0);

                    let mut best_c = ci_r;
                    let mut best_gain = stay_gain;
                    for &cand in &touched {
                        if cand == ci_r {
                            continue;
                        }
                        let k_i_c = k_i_to.get(&cand).copied().unwrap_or(0.0);
                        let sigma_c = sigma_tot.get(&cand).copied().unwrap_or(0.0);
                        let gain = k_i_c / (two_m / 2.0) - ki * sigma_c / (two_m * two_m / 2.0);
                        if gain > best_gain + config.min_modularity_gain {
                            best_gain = gain;
                            best_c = cand;
                        }
                    }

                    refined[i as usize].store(best_c, Ordering::Relaxed);
                    *sigma_tot.entry(best_c).or_insert(0.0) += ki;
                    if best_c != ci_r {
                        iter_moved = true;
                    }

                    for &cand in &touched {
                        k_i_to.insert(cand, 0.0);
                    }
                    touched.clear();
                }
                if !iter_moved {
                    break;
                }
            }
        });

    refined.into_iter().map(|a| a.into_inner()).collect()
}

/// Build the aggregated super-graph: each refined sub-community becomes a
/// super-node; super-edges sum the inter-community edge weights from `adj`.
fn aggregate(
    adj: &[Vec<(u32, f64)>],
    degree: &[f64],
    refined: &[u32],
    refined_id_map: &FxHashMap<u32, u32>,
) -> (Vec<Vec<(u32, f64)>>, Vec<f64>) {
    let m = refined_id_map.len();
    let mut super_adj_map: Vec<FxHashMap<u32, f64>> = vec![FxHashMap::default(); m];
    let mut super_degree: Vec<f64> = vec![0.0; m];

    for (i, nbrs) in adj.iter().enumerate() {
        let ci = refined_id_map[&refined[i]] as usize;
        super_degree[ci] += degree[i];
        for &(j, w) in nbrs {
            let cj = refined_id_map[&refined[j as usize]] as usize;
            if ci == cj {
                continue; // self-loop handled implicitly by super_degree
            }
            *super_adj_map[ci].entry(cj as u32).or_insert(0.0) += w;
        }
    }

    let super_adj: Vec<Vec<(u32, f64)>> = super_adj_map
        .into_iter()
        .map(|h| h.into_iter().collect())
        .collect();
    (super_adj, super_degree)
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
            content_hash: 0,
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
    fn star_hub_lands_in_one_chain_not_singleton() {
        // hub + 4 chains of length 2 — the canonical badly-connected-hub case.
        // hub: 0; chains: (1,2), (3,4), (5,6), (7,8)
        let mut pool = StringPool::new();
        let nodes: Vec<Node> = (0..9)
            .map(|i| n(&mut pool, &format!("f{i}"), NodeKind::Function))
            .collect();
        let edges = vec![
            e(0, 1, RelType::Calls),
            e(1, 2, RelType::Calls),
            e(0, 3, RelType::Calls),
            e(3, 4, RelType::Calls),
            e(0, 5, RelType::Calls),
            e(5, 6, RelType::Calls),
            e(0, 7, RelType::Calls),
            e(7, 8, RelType::Calls),
        ];
        let cfg = LeidenConfig::default();
        let a = detect_communities(&nodes, &edges, &cfg);

        // The hub (node 0) must share a community with at least one of its
        // chain neighbors — Leiden's refinement guarantees this. Louvain
        // typically pins it to whichever chain it touched first and stops.
        let hub_comm = a[0];
        let chain_partners: Vec<u32> = vec![1, 3, 5, 7];
        let shares = chain_partners.iter().any(|&p| a[p as usize] == hub_comm);
        assert!(
            shares,
            "hub should share a community with ≥1 chain neighbor; assignments={a:?}"
        );

        // No node should be unassigned.
        for (i, &c) in a.iter().enumerate() {
            assert_ne!(c, 0, "node {i} unassigned");
        }
    }

    #[test]
    fn two_disconnected_triangles_form_two_communities() {
        let mut pool = StringPool::new();
        let nodes: Vec<Node> = (0..6)
            .map(|i| n(&mut pool, &format!("f{i}"), NodeKind::Function))
            .collect();
        let edges = vec![
            e(0, 1, RelType::Calls),
            e(1, 2, RelType::Calls),
            e(0, 2, RelType::Calls),
            e(3, 4, RelType::Calls),
            e(4, 5, RelType::Calls),
            e(3, 5, RelType::Calls),
        ];
        let cfg = LeidenConfig::default();
        let a = detect_communities(&nodes, &edges, &cfg);
        assert_eq!(a[0], a[1]);
        assert_eq!(a[1], a[2]);
        assert_eq!(a[3], a[4]);
        assert_eq!(a[4], a[5]);
        assert_ne!(a[0], a[3]);
    }

    #[test]
    fn empty_graph_returns_empty() {
        let cfg = LeidenConfig::default();
        let r = detect_communities(&[], &[], &cfg);
        assert!(r.is_empty());
    }
}
