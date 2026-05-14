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

/// 共用中介表示：markdown 與 json 渲染都只需這幾個欄位，避免重複算 shadowed_by。
struct SymbolEntry<'a> {
    name: &'a str,
    kind: &'static str,
    in_deg: u32,
    /// 0 = 此 name 唯一；N>0 = 另有 N 處同名節點
    shadowed_by: usize,
}

fn file_path(g: &ArchivedZeroCopyGraph, file_idx: u32) -> String {
    let i = file_idx as usize;
    if i >= g.files.len() {
        return format!("<unknown file_idx={i}>");
    }
    g.files[i].path.resolve(&g.string_pool).to_string()
}

fn nodes_in_file<'a>(input: &'a RenderInput<'a>, file_idx: u32) -> &'a [usize] {
    input
        .by_file
        .get(&file_idx)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// 對一個 file 取出 top-K symbol 的中介表示。markdown / json 共用。
fn resolve_symbols<'a>(input: &'a RenderInput<'a>, file_idx: u32) -> Vec<SymbolEntry<'a>> {
    let nodes = nodes_in_file(input, file_idx);
    let picked = super::ranking::top_symbols_in_file(
        nodes,
        input.stats,
        input.top_symbols_per_file,
        input.exclude_orphans,
    );
    picked
        .into_iter()
        .map(|i| {
            let name = input.graph.nodes[i].name.resolve(&input.graph.string_pool);
            let occurrences = input.name_collisions.get(name).map(Vec::len).unwrap_or(1);
            SymbolEntry {
                name,
                kind: kind_to_str(&input.graph.nodes[i].kind),
                in_deg: input.stats.in_deg[i],
                shadowed_by: occurrences.saturating_sub(1),
            }
        })
        .collect()
}

pub fn markdown(input: &RenderInput) -> String {
    let g = input.graph;
    // 容量估算：header ~512 + top_files × (~64 base + symbols × ~80) + top_communities × ~80
    let capacity = 512
        + input.top_files.len() * (64 + input.top_symbols_per_file * 80)
        + input.top_communities.len() * 80;
    let mut out = String::with_capacity(capacity);

    writeln!(out, "# Project Summary\n").unwrap();
    writeln!(
        out,
        "Files: {}  •  Symbols: {}  •  Communities: {}\n",
        g.files.len(),
        g.nodes.len(),
        input.total_communities,
    )
    .unwrap();

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

    writeln!(out, "## Per-file detail\n").unwrap();
    let total_files = input.by_file.len();
    let shown = input.top_files.len();
    for fs in input.top_files.iter() {
        let path = file_path(g, fs.file_idx);
        let file_nodes = nodes_in_file(input, fs.file_idx);
        let comm_id = file_nodes
            .first()
            .map(|&i| g.nodes[i].community_id.to_native())
            .unwrap_or(0);
        let comm_tag = if comm_id == 0 {
            String::new()
        } else {
            format!("  [community {comm_id}]")
        };
        writeln!(out, "### `{path}`{comm_tag}").unwrap();

        let symbols = resolve_symbols(input, fs.file_idx);
        if symbols.is_empty() {
            writeln!(out, "_(no non-orphan symbols)_\n").unwrap();
            continue;
        }
        for s in &symbols {
            let shadowed = if s.shadowed_by > 0 {
                format!(" ← shadowed by {} same-name", s.shadowed_by)
            } else {
                String::new()
            };
            writeln!(
                out,
                "- {} `{}` (in_deg={}){}",
                s.kind, s.name, s.in_deg, shadowed
            )
            .unwrap();
        }
        let extra = file_nodes.len().saturating_sub(symbols.len());
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
            let symbols: Vec<_> = resolve_symbols(input, fs.file_idx)
                .into_iter()
                .map(|s| {
                    serde_json::json!({
                        "name": s.name,
                        "kind": s.kind,
                        "in_deg": s.in_deg,
                        "shadowed_by": s.shadowed_by,
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
