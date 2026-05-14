//! Pure data-extraction passes over `ArchivedZeroCopyGraph`.
//! Everything here is O(N + E) and side-effect-free.

use gnx_core::graph::ArchivedZeroCopyGraph;
use std::collections::{BTreeMap, HashMap};

/// In/out degree per node index, plus a centrality signal
/// (`cross_community_in_deg`) counting only incoming edges that *cross* a
/// community boundary.
///
/// `cross_community_in_deg[i]` is the count of edges whose target is `i` AND
/// whose source's `community_id` differs from `i`'s `community_id`. Edges into
/// orphan community 0 are still counted across distinct communities. This
/// signal upranks files acting as bridges / public API surface and downranks
/// files that only receive traffic from their own tightly-coupled cluster
/// (e.g. vendored grammar internals).
#[derive(Debug, Clone, Default)]
pub struct DegreeStats {
    pub in_deg: Vec<u32>,
    pub out_deg: Vec<u32>,
    pub cross_community_in_deg: Vec<u32>,
}

pub fn degree_stats(g: &ArchivedZeroCopyGraph) -> DegreeStats {
    let n = g.nodes.len();
    let mut in_deg = vec![0u32; n];
    let mut out_deg = vec![0u32; n];
    let mut cross_community_in_deg = vec![0u32; n];
    for e in g.edges.iter() {
        let s = e.source.to_native() as usize;
        let t = e.target.to_native() as usize;
        if s < n {
            out_deg[s] = out_deg[s].saturating_add(1);
        }
        if t < n {
            in_deg[t] = in_deg[t].saturating_add(1);
            if s < n && g.nodes[s].community_id.to_native() != g.nodes[t].community_id.to_native() {
                cross_community_in_deg[t] = cross_community_in_deg[t].saturating_add(1);
            }
        }
    }
    DegreeStats {
        in_deg,
        out_deg,
        cross_community_in_deg,
    }
}

/// Group node indices by file_idx. BTreeMap keeps deterministic file ordering.
pub fn by_file(g: &ArchivedZeroCopyGraph) -> BTreeMap<u32, Vec<usize>> {
    let mut map: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (i, n) in g.nodes.iter().enumerate() {
        map.entry(n.file_idx.to_native()).or_default().push(i);
    }
    map
}

/// Group node indices by community_id. 0 = unassigned.
pub fn by_community(g: &ArchivedZeroCopyGraph) -> BTreeMap<u16, Vec<usize>> {
    let mut map: BTreeMap<u16, Vec<usize>> = BTreeMap::new();
    for (i, n) in g.nodes.iter().enumerate() {
        map.entry(n.community_id.to_native()).or_default().push(i);
    }
    map
}

/// Names that occur on ≥2 distinct nodes. Used to flag shadowed symbols.
///
/// 兩段式：先用借用 &str 計數（不 allocate），第二段才為碰撞 name allocate
/// String + Vec。大型 repo 中 unique name 比例高（>90%）時可省 ~95% String alloc。
pub fn name_collisions(g: &ArchivedZeroCopyGraph) -> HashMap<String, Vec<usize>> {
    let mut counts: HashMap<&str, usize> = HashMap::with_capacity(g.nodes.len());
    for n in g.nodes.iter() {
        *counts.entry(n.name.resolve(&g.string_pool)).or_default() += 1;
    }
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, n) in g.nodes.iter().enumerate() {
        let name = n.name.resolve(&g.string_pool);
        if counts.get(name).copied().unwrap_or(0) >= 2 {
            by_name.entry(name.to_string()).or_default().push(i);
        }
    }
    by_name
}

#[cfg(test)]
mod tests {
    // 真實 ArchivedZeroCopyGraph fixture 需 mmap一個產出的 graph.bin。
    // 這些 fn 都是純線性 reducer，整合測試在 commands/summarize/mod.rs::tests
    // 對小型 fixture 覆蓋。此處留 hook。
}
