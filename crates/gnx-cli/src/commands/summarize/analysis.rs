//! Pure data-extraction passes over `ArchivedZeroCopyGraph`.
//! Everything here is O(N + E) and side-effect-free.

use gnx_core::graph::ArchivedZeroCopyGraph;
use std::collections::{BTreeMap, HashMap};

/// In/out degree per node index.
#[derive(Debug, Clone, Default)]
pub struct DegreeStats {
    pub in_deg: Vec<u32>,
    pub out_deg: Vec<u32>,
}

pub fn degree_stats(g: &ArchivedZeroCopyGraph) -> DegreeStats {
    let n = g.nodes.len();
    let mut in_deg = vec![0u32; n];
    let mut out_deg = vec![0u32; n];
    for e in g.edges.iter() {
        let s = e.source.to_native() as usize;
        let t = e.target.to_native() as usize;
        if s < n {
            out_deg[s] = out_deg[s].saturating_add(1);
        }
        if t < n {
            in_deg[t] = in_deg[t].saturating_add(1);
        }
    }
    DegreeStats { in_deg, out_deg }
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
pub fn name_collisions(g: &ArchivedZeroCopyGraph) -> HashMap<String, Vec<usize>> {
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, n) in g.nodes.iter().enumerate() {
        let name = n.name.resolve(&g.string_pool).to_string();
        by_name.entry(name).or_default().push(i);
    }
    by_name.retain(|_, v| v.len() >= 2);
    by_name
}

#[cfg(test)]
mod tests {
    // 真實 ArchivedZeroCopyGraph fixture 需 mmap一個產出的 graph.bin。
    // 這些 fn 都是純線性 reducer，整合測試在 commands/summarize/mod.rs::tests
    // 對小型 fixture 覆蓋。此處留 hook。
}
