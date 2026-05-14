//! Sort + truncate helpers. Pure functions over node indices.

use super::analysis::DegreeStats;
use gnx_core::graph::ArchivedZeroCopyGraph;
use std::collections::BTreeMap;

/// File-level summary: aggregate symbol count, sum of in_deg, and sum of
/// cross-community in_deg across the file's symbols.
#[derive(Debug, Clone)]
pub struct FileSummary {
    pub file_idx: u32,
    pub symbol_count: usize,
    pub total_in_deg: u32,
    /// Primary ranking signal — sum of incoming edges from nodes in a
    /// *different* community than the receiving node. Counts "bridge" traffic
    /// that crosses module boundaries, which is a better proxy for "core /
    /// public API" than raw `in_deg` (which inflates with tightly-coupled
    /// internal call chains, e.g. vendored grammar helpers).
    pub cross_community_in_deg: u32,
}

/// Top-N files ranked by centrality.
///
/// Sort key (descending):
///   1. `cross_community_in_deg` — bridge-traffic centrality.
///   2. `total_in_deg` — general inbound traffic (degrades gracefully when the
///      whole repo lands in a single community, e.g. very small projects).
///   3. `symbol_count` — bigger surface area wins ties.
///   4. `file_idx` — deterministic tiebreak.
///
/// 用 `select_nth_unstable_by` 做 partial-sort (O(n) + O(k log k))，避免對全
/// 集合 sort_by(O(n log n))。對大型 repo 收斂 K << N 時差異顯著。
pub fn top_files(
    by_file: &BTreeMap<u32, Vec<usize>>,
    stats: &DegreeStats,
    top_n: usize,
) -> Vec<FileSummary> {
    let mut summaries: Vec<FileSummary> = by_file
        .iter()
        .map(|(&fi, nodes)| {
            let total_in_deg: u32 = nodes.iter().map(|&i| stats.in_deg[i]).sum();
            let cross_community_in_deg: u32 =
                nodes.iter().map(|&i| stats.cross_community_in_deg[i]).sum();
            FileSummary {
                file_idx: fi,
                symbol_count: nodes.len(),
                total_in_deg,
                cross_community_in_deg,
            }
        })
        .collect();
    if top_n == 0 {
        return Vec::new();
    }
    let cmp = |a: &FileSummary, b: &FileSummary| {
        b.cross_community_in_deg
            .cmp(&a.cross_community_in_deg)
            .then_with(|| b.total_in_deg.cmp(&a.total_in_deg))
            .then_with(|| b.symbol_count.cmp(&a.symbol_count))
            .then_with(|| a.file_idx.cmp(&b.file_idx))
    };
    if top_n < summaries.len() {
        summaries.select_nth_unstable_by(top_n - 1, cmp);
        summaries.truncate(top_n);
    }
    summaries.sort_by(cmp);
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
    if top_n == 0 {
        return Vec::new();
    }
    let cmp = |a: &CommunitySummary, b: &CommunitySummary| {
        b.symbol_count
            .cmp(&a.symbol_count)
            .then_with(|| b.file_count.cmp(&a.file_count))
            .then_with(|| a.community_id.cmp(&b.community_id))
    };
    if top_n < summaries.len() {
        summaries.select_nth_unstable_by(top_n - 1, cmp);
        summaries.truncate(top_n);
    }
    summaries.sort_by(cmp);
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
    if top_k == 0 {
        return Vec::new();
    }
    let cmp = |&a: &usize, &b: &usize| {
        stats.in_deg[b]
            .cmp(&stats.in_deg[a])
            .then_with(|| a.cmp(&b))
    };
    if top_k < candidates.len() {
        candidates.select_nth_unstable_by(top_k - 1, cmp);
        candidates.truncate(top_k);
    }
    candidates.sort_by(cmp);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_stats(in_deg: Vec<u32>, out_deg: Vec<u32>) -> DegreeStats {
        let n = in_deg.len();
        DegreeStats {
            in_deg,
            out_deg,
            cross_community_in_deg: vec![0; n],
        }
    }

    fn fake_stats_with_xc(
        in_deg: Vec<u32>,
        out_deg: Vec<u32>,
        cross_community_in_deg: Vec<u32>,
    ) -> DegreeStats {
        DegreeStats {
            in_deg,
            out_deg,
            cross_community_in_deg,
        }
    }

    #[test]
    fn top_files_orders_by_aggregated_in_deg_then_symbol_count() {
        // file 0: 2 nodes with in_deg [10, 5] → total 15
        // file 1: 1 node with in_deg [20] → total 20
        // file 2: 3 nodes with in_deg [5, 5, 5] → total 15 (ties file 0, but more symbols)
        let stats = fake_stats(vec![10, 5, 20, 5, 5, 5], vec![0, 0, 0, 0, 0, 0]);
        let mut by_file: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        by_file.insert(0, vec![0, 1]);
        by_file.insert(1, vec![2]);
        by_file.insert(2, vec![3, 4, 5]);
        let result = top_files(&by_file, &stats, 10);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].file_idx, 1); // total=20 wins
        assert_eq!(result[1].file_idx, 2); // total=15, 3 symbols > file 0's 2
        assert_eq!(result[2].file_idx, 0);
    }

    #[test]
    fn top_files_partial_sort_returns_correct_top_k() {
        // 6 檔，in_deg 分別 [60,50,40,30,20,10]，top_n=3 應拿前 3 大且 sorted。
        let stats = fake_stats(vec![60, 50, 40, 30, 20, 10], vec![0; 6]);
        let mut by_file: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        for i in 0..6 {
            by_file.insert(i as u32, vec![i]);
        }
        let result = top_files(&by_file, &stats, 3);
        let totals: Vec<u32> = result.iter().map(|f| f.total_in_deg).collect();
        assert_eq!(totals, vec![60, 50, 40]);
    }

    #[test]
    fn top_files_ranks_by_cross_community_first() {
        // file 0: in_deg [50], cross_community [5]  → primary key 5
        // file 1: in_deg [100], cross_community [0] → primary key 0 (loses despite higher in_deg)
        // file 2: in_deg [20], cross_community [20] → primary key 20 (wins)
        // Pin: cross_community_in_deg dominates total_in_deg in ranking, so a
        // file with low overall traffic but high bridge-traffic outranks a
        // popular file whose callers are all internal to its own community.
        let stats = fake_stats_with_xc(vec![50, 100, 20], vec![0, 0, 0], vec![5, 0, 20]);
        let mut by_file: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        by_file.insert(0, vec![0]);
        by_file.insert(1, vec![1]);
        by_file.insert(2, vec![2]);
        let result = top_files(&by_file, &stats, 10);
        assert_eq!(result[0].file_idx, 2);
        assert_eq!(result[1].file_idx, 0);
        assert_eq!(result[2].file_idx, 1);
    }

    #[test]
    fn top_files_falls_back_to_in_deg_when_no_cross_community() {
        // All files single-community (cross_community all zero) → tiebreaker
        // walks to total_in_deg. Pins graceful degradation for trivially small
        // single-cluster repos where centrality signal is degenerate.
        let stats = fake_stats(vec![10, 20, 5], vec![0, 0, 0]);
        let mut by_file: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        by_file.insert(0, vec![0]);
        by_file.insert(1, vec![1]);
        by_file.insert(2, vec![2]);
        let result = top_files(&by_file, &stats, 10);
        assert_eq!(result[0].file_idx, 1);
        assert_eq!(result[1].file_idx, 0);
        assert_eq!(result[2].file_idx, 2);
    }

    #[test]
    fn top_files_top_n_zero_returns_empty() {
        // 對應 spec：--top-files=0 視為「不輸出此 section」，回傳空 Vec。
        let stats = fake_stats(vec![5, 5], vec![0, 0]);
        let mut by_file: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        by_file.insert(0, vec![0]);
        by_file.insert(1, vec![1]);
        assert!(top_files(&by_file, &stats, 0).is_empty());
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
