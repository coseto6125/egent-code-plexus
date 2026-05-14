//! Blind-spot aggregation for the summary.
//!
//! Each `BlindSpotRecord` marks a site where static analysis could not follow
//! the call (Python `eval`/`exec`/dynamic import/cross-`getattr`, …). The LLM
//! reading the summary needs to know how much of the codebase relies on
//! reflection so it can calibrate trust in the graph.

use graph_nexus_core::graph::ArchivedZeroCopyGraph;
use std::collections::HashMap;

/// Top-N files default for the blind-spot "top files" breakdown. Documented
/// constant — actual wiring reuses `SummarizeArgs::top_files` so the user
/// flag flows through; this const exists to pin the contract.
#[allow(dead_code)]
pub const DEFAULT_TOP_FILES: usize = 10;

/// Aggregated blind-spot statistics, ready for render.
#[derive(Debug, Clone, Default)]
pub struct BlindSpotStats {
    pub total: usize,
    /// Distinct files containing ≥1 blind spot (separate from `top_files.len()`
    /// because top_files is truncated to top-N).
    pub distinct_files: usize,
    /// kind → count, sorted by count desc, then kind asc for deterministic
    /// output.
    pub by_kind: Vec<(String, usize)>,
    /// file_path → count, top-N sorted by count desc, then path asc.
    pub top_files: Vec<(String, usize)>,
}

/// Single-pass O(N) reducer over `graph.blind_spots`. Borrows strings from
/// the string pool while counting; only allocates `String` keys for the
/// final aggregates (kind cardinality is tiny, file cardinality bounded by
/// distinct files containing reflection).
pub fn collect(g: &ArchivedZeroCopyGraph, top_files: usize) -> BlindSpotStats {
    let total = g.blind_spots.len();
    if total == 0 {
        return BlindSpotStats::default();
    }

    let mut by_kind: HashMap<&str, usize> = HashMap::new();
    let mut by_file: HashMap<&str, usize> = HashMap::new();
    for bs in g.blind_spots.iter() {
        *by_kind.entry(bs.kind.resolve(&g.string_pool)).or_default() += 1;
        *by_file
            .entry(bs.file_path.resolve(&g.string_pool))
            .or_default() += 1;
    }

    let mut by_kind: Vec<(String, usize)> = by_kind
        .into_iter()
        .map(|(k, c)| (k.to_string(), c))
        .collect();
    by_kind.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let distinct_files = by_file.len();
    let mut top_files_vec: Vec<(String, usize)> = by_file
        .into_iter()
        .map(|(p, c)| (p.to_string(), c))
        .collect();
    let cmp = |a: &(String, usize), b: &(String, usize)| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0));
    if top_files > 0 && top_files < top_files_vec.len() {
        top_files_vec.select_nth_unstable_by(top_files - 1, cmp);
        top_files_vec.truncate(top_files);
    }
    top_files_vec.sort_by(cmp);

    BlindSpotStats {
        total,
        distinct_files,
        by_kind,
        top_files: top_files_vec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_returns_zero_stats() {
        let stats = BlindSpotStats::default();
        assert_eq!(stats.total, 0);
        assert!(stats.by_kind.is_empty());
        assert!(stats.top_files.is_empty());
    }

    #[test]
    fn default_top_files_constant_is_ten() {
        // Pin: per task spec, top-N defaults to 10 when no flag is wired through.
        assert_eq!(DEFAULT_TOP_FILES, 10);
    }
}
