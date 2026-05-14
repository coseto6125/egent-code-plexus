//! Render the analysis result into LLM-friendly markdown or JSON.

use super::analysis::DegreeStats;
use super::ranking::{CommunitySummary, FileSummary};
use crate::commands::format::kind_to_str;
use gnx_core::graph::ArchivedZeroCopyGraph;
use std::collections::HashMap;
use std::fmt::Write;

pub struct RenderInput<'a> {
    pub graph: &'a ArchivedZeroCopyGraph,
    pub stats: &'a DegreeStats,
    pub by_file: &'a std::collections::BTreeMap<u32, Vec<usize>>,
    pub top_files: &'a [FileSummary],
    pub top_communities: &'a [CommunitySummary],
    /// 總 community 數（含 unassigned，僅供 header 顯示）
    pub total_communities: usize,
    pub name_collisions: &'a HashMap<String, Vec<usize>>,
    pub top_symbols_per_file: usize,
    pub exclude_orphans: bool,
}

fn file_path(g: &ArchivedZeroCopyGraph, file_idx: u32) -> String {
    let i = file_idx as usize;
    if i >= g.files.len() {
        return format!("<unknown file_idx={i}>");
    }
    g.files[i].path.resolve(&g.string_pool).to_string()
}

fn node_name(g: &ArchivedZeroCopyGraph, idx: usize) -> &str {
    g.nodes[idx].name.resolve(&g.string_pool)
}

pub fn markdown(input: &RenderInput) -> String {
    let g = input.graph;
    let mut out = String::with_capacity(4096);

    // Header
    writeln!(out, "# Project Summary\n").unwrap();
    writeln!(
        out,
        "Files: {}  •  Symbols: {}  •  Communities: {}\n",
        g.files.len(),
        g.nodes.len(),
        input.total_communities,
    )
    .unwrap();

    // Top hot files
    writeln!(out, "## Top hot files\n").unwrap();
    if input.top_files.is_empty() {
        writeln!(out, "_(no files indexed)_\n").unwrap();
    } else {
        for (rank, fs) in input.top_files.iter().enumerate() {
            writeln!(
                out,
                "{}. `{}` — {} symbol{}, {} aggregated in_deg",
                rank + 1,
                file_path(g, fs.file_idx),
                fs.symbol_count,
                if fs.symbol_count == 1 { "" } else { "s" },
                fs.total_in_deg,
            )
            .unwrap();
        }
        writeln!(out).unwrap();
    }

    // Architecture
    writeln!(out, "## Architecture (top communities)\n").unwrap();
    if input.top_communities.is_empty() {
        writeln!(out, "_(no communities detected)_\n").unwrap();
    } else {
        for cs in input.top_communities.iter() {
            let label = if cs.community_id == 0 {
                "Unassigned".to_string()
            } else {
                format!("Community {}", cs.community_id)
            };
            let anchor = cs
                .anchor_file_idx
                .map(|fi| format!(", anchor: `{}`", file_path(g, fi)))
                .unwrap_or_default();
            writeln!(
                out,
                "- {label} — {} symbols across {} file{}{anchor}",
                cs.symbol_count,
                cs.file_count,
                if cs.file_count == 1 { "" } else { "s" },
            )
            .unwrap();
        }
        writeln!(out).unwrap();
    }

    // Per-file detail (limited to the same top_files)
    writeln!(out, "## Per-file detail\n").unwrap();
    let total_files = input.by_file.len();
    let shown = input.top_files.len();
    for fs in input.top_files.iter() {
        let path = file_path(g, fs.file_idx);
        let nodes = input.by_file.get(&fs.file_idx).cloned().unwrap_or_default();
        // pick community of dominant node
        let comm_id = nodes
            .first()
            .map(|&i| g.nodes[i].community_id.to_native())
            .unwrap_or(0);
        let comm_tag = if comm_id == 0 {
            String::new()
        } else {
            format!("  [community {comm_id}]")
        };
        writeln!(out, "### `{path}`{comm_tag}").unwrap();

        let picked = super::ranking::top_symbols_in_file(
            &nodes,
            input.stats,
            input.top_symbols_per_file,
            input.exclude_orphans,
        );
        if picked.is_empty() {
            writeln!(out, "_(no non-orphan symbols)_\n").unwrap();
            continue;
        }
        for &i in &picked {
            let name = node_name(g, i);
            let kind = kind_to_str(&g.nodes[i].kind);
            let in_deg = input.stats.in_deg[i];
            let shadowed = input
                .name_collisions
                .get(name)
                .map(|v| v.len())
                .filter(|&n| n > 1)
                .map(|n| format!(" ← shadowed by {} same-name", n - 1))
                .unwrap_or_default();
            writeln!(out, "- {kind} `{name}` (in_deg={in_deg}){shadowed}").unwrap();
        }
        let extra = nodes.len().saturating_sub(picked.len());
        if extra > 0 {
            writeln!(out, "- _… +{extra} more_").unwrap();
        }
        writeln!(out).unwrap();
    }
    if total_files > shown {
        writeln!(
            out,
            "_… (truncated; {} more files; rerun with --top-files <N>)_",
            total_files - shown
        )
        .unwrap();
    }

    out
}

pub fn json(input: &RenderInput) -> serde_json::Value {
    let g = input.graph;
    let top_files: Vec<_> = input
        .top_files
        .iter()
        .map(|fs| {
            let nodes = input.by_file.get(&fs.file_idx).cloned().unwrap_or_default();
            let picked = super::ranking::top_symbols_in_file(
                &nodes,
                input.stats,
                input.top_symbols_per_file,
                input.exclude_orphans,
            );
            let symbols: Vec<_> = picked
                .iter()
                .map(|&i| {
                    let name = node_name(g, i).to_string();
                    let shadowed = input
                        .name_collisions
                        .get(&name)
                        .map(|v| v.len())
                        .unwrap_or(1);
                    serde_json::json!({
                        "name": name,
                        "kind": kind_to_str(&g.nodes[i].kind),
                        "in_deg": input.stats.in_deg[i],
                        "shadowed_by": shadowed.saturating_sub(1),
                    })
                })
                .collect();
            serde_json::json!({
                "path": file_path(g, fs.file_idx),
                "symbol_count": fs.symbol_count,
                "total_in_deg": fs.total_in_deg,
                "top_symbols": symbols,
            })
        })
        .collect();

    let communities: Vec<_> = input
        .top_communities
        .iter()
        .map(|cs| {
            serde_json::json!({
                "community_id": cs.community_id,
                "symbol_count": cs.symbol_count,
                "file_count": cs.file_count,
                "anchor_file": cs.anchor_file_idx.map(|fi| file_path(g, fi)),
            })
        })
        .collect();

    serde_json::json!({
        "files_total": g.files.len(),
        "symbols_total": g.nodes.len(),
        "top_files": top_files,
        "top_communities": communities,
        "truncated_file_count": input.by_file.len().saturating_sub(input.top_files.len()),
    })
}
