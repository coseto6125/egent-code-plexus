//! Sort + truncate helpers. Pure functions over node indices.

use super::analysis::DegreeStats;
use gnx_core::graph::ArchivedZeroCopyGraph;
use std::collections::BTreeMap;

/// File-level summary: aggregate symbol count + sum of in_deg across symbols.
#[derive(Debug, Clone)]
pub struct FileSummary {
    pub file_idx: u32,
    pub symbol_count: usize,
    pub total_in_deg: u32,
}

/// Top-N files ordered by aggregated in_deg (descending), then by symbol_count.
pub fn top_files(
    by_file: &BTreeMap<u32, Vec<usize>>,
    stats: &DegreeStats,
    top_n: usize,
) -> Vec<FileSummary> {
    let mut summaries: Vec<FileSummary> = by_file
        .iter()
        .map(|(&fi, nodes)| {
            let total_in_deg: u32 = nodes.iter().map(|&i| stats.in_deg[i]).sum();
            FileSummary {
                file_idx: fi,
                symbol_count: nodes.len(),
                total_in_deg,
            }
        })
        .collect();
    summaries.sort_by(|a, b| {
        b.total_in_deg
            .cmp(&a.total_in_deg)
            .then_with(|| b.symbol_count.cmp(&a.symbol_count))
            .then_with(|| a.file_idx.cmp(&b.file_idx))
    });
    summaries.truncate(top_n);
    summaries
}

/// Community-level summary.
#[derive(Debug, Clone)]
pub struct CommunitySummary {
    pub community_id: u16,
    pub symbol_count: usize,
    pub file_count: usize,
    /// Anchor file = most-represented file in this community.
    pub anchor_file_idx: Option<u32>,
}

/// Top-N communities by symbol count.
pub fn top_communities(
    g: &ArchivedZeroCopyGraph,
    by_community: &BTreeMap<u16, Vec<usize>>,
    top_n: usize,
) -> Vec<CommunitySummary> {
    let mut summaries: Vec<CommunitySummary> = by_community
        .iter()
        .map(|(&cid, nodes)| {
            let mut file_count: BTreeMap<u32, usize> = BTreeMap::new();
            for &i in nodes {
                *file_count
                    .entry(g.nodes[i].file_idx.to_native())
                    .or_default() += 1;
            }
            let anchor = file_count.iter().max_by_key(|&(_, c)| *c).map(|(f, _)| *f);
            CommunitySummary {
                community_id: cid,
                symbol_count: nodes.len(),
                file_count: file_count.len(),
                anchor_file_idx: anchor,
            }
        })
        .collect();
    summaries.sort_by(|a, b| {
        b.symbol_count
            .cmp(&a.symbol_count)
            .then_with(|| b.file_count.cmp(&a.file_count))
            .then_with(|| a.community_id.cmp(&b.community_id))
    });
    summaries.truncate(top_n);
    summaries
}

/// For a given file, return top-K symbol node indices.
/// `exclude_orphans` drops nodes where in_deg == 0 && out_deg == 0.
pub fn top_symbols_in_file(
    nodes: &[usize],
    stats: &DegreeStats,
    top_k: usize,
    exclude_orphans: bool,
) -> Vec<usize> {
    let mut candidates: Vec<usize> = if exclude_orphans {
        nodes
            .iter()
            .copied()
            .filter(|&i| stats.in_deg[i] != 0 || stats.out_deg[i] != 0)
            .collect()
    } else {
        nodes.to_vec()
    };
    candidates.sort_by(|&a, &b| {
        stats.in_deg[b]
            .cmp(&stats.in_deg[a])
            .then_with(|| a.cmp(&b))
    });
    candidates.truncate(top_k);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_stats(in_deg: Vec<u32>, out_deg: Vec<u32>) -> DegreeStats {
        DegreeStats { in_deg, out_deg }
    }

    #[test]
    fn top_symbols_orphan_filter_drops_isolated_nodes() {
        // node 0: orphan, node 1: in_deg=5, node 2: out_deg=2
        let stats = fake_stats(vec![0, 5, 0], vec![0, 0, 2]);
        let result = top_symbols_in_file(&[0, 1, 2], &stats, 10, true);
        assert_eq!(result, vec![1, 2]); // 0 excluded
    }

    #[test]
    fn top_symbols_includes_orphans_when_flag_off() {
        // node 1 in_deg=5 → first; node 0 & 2 tied at in_deg=0, idx asc → 0, 2
        let stats = fake_stats(vec![0, 5, 0], vec![0, 0, 2]);
        let result = top_symbols_in_file(&[0, 1, 2], &stats, 10, false);
        assert_eq!(result, vec![1, 0, 2]);
    }

    #[test]
    fn top_symbols_truncates_to_k() {
        let stats = fake_stats(vec![5, 3, 1, 0], vec![0, 0, 0, 0]);
        let result = top_symbols_in_file(&[0, 1, 2, 3], &stats, 2, false);
        assert_eq!(result, vec![0, 1]);
    }
}
