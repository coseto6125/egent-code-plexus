//! POC for `cgn summarize` — three rendering strategies on top of graph.bin.
//!
//! Usage:
//!   summarize_poc <graph.bin> <A|B|C> [top_k]
//!
//! A = file tree + 1 representative symbol/file
//! B = file tree + top-K symbols/file (default K=5)
//! C = community-grouped overview

use cgn_core::graph::ArchivedZeroCopyGraph;
use memmap2::Mmap;
use std::collections::BTreeMap;
use std::fs::File;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .ok_or("usage: summarize_poc <graph.bin> <A|B|C> [top_k]")?;
    let format = args.get(2).cloned().unwrap_or_else(|| "B".to_string());
    let top_k: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(5);

    let f = File::open(&path)?;
    let mmap = unsafe { Mmap::map(&f)? };
    let g = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&mmap)
        .map_err(|e| format!("{e}"))?;

    // 預先計算每個 node 的 in-degree（重要性近似指標）
    let n_nodes = g.nodes.len();
    let mut in_deg: Vec<u32> = vec![0; n_nodes];
    for e in g.edges.iter() {
        let tgt = e.target.to_native() as usize;
        if tgt < n_nodes {
            in_deg[tgt] = in_deg[tgt].saturating_add(1);
        }
    }

    // 把 node 依 file 聚合
    let mut by_file: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (i, n) in g.nodes.iter().enumerate() {
        by_file.entry(n.file_idx.to_native()).or_default().push(i);
    }

    match format.as_str() {
        "A" => render_a(g, &by_file, &in_deg),
        "B" => render_b(g, &by_file, &in_deg, top_k),
        "C" => render_c(g, &in_deg, top_k),
        _ => return Err("format must be A|B|C".into()),
    }
    Ok(())
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

fn kind_str(g: &ArchivedZeroCopyGraph, idx: usize) -> String {
    format!("{:?}", g.nodes[idx].kind)
}

/// A: 一個檔一個代表 symbol（in-degree 最大者）
fn render_a(g: &ArchivedZeroCopyGraph, by_file: &BTreeMap<u32, Vec<usize>>, in_deg: &[u32]) {
    println!("# Project Summary (A: file tree + representative symbol)\n");
    println!(
        "Total: {} files, {} symbols\n",
        by_file.len(),
        g.nodes.len()
    );
    for (&fi, nodes) in by_file {
        let path = file_path(g, fi);
        let rep = nodes.iter().copied().max_by_key(|&i| in_deg[i]);
        match rep {
            Some(r) => println!("- `{}` — {} `{}`", path, kind_str(g, r), node_name(g, r)),
            None => println!("- `{}`", path),
        }
    }
}

/// B: 每檔展開 top-K 符號（依 in-degree 降序，平手取 alphabetical）
fn render_b(
    g: &ArchivedZeroCopyGraph,
    by_file: &BTreeMap<u32, Vec<usize>>,
    in_deg: &[u32],
    top_k: usize,
) {
    println!("# Project Summary (B: file tree + top-{top_k} symbols per file)\n");
    println!(
        "Total: {} files, {} symbols\n",
        by_file.len(),
        g.nodes.len()
    );
    for (&fi, nodes) in by_file {
        let path = file_path(g, fi);
        println!("## `{}`", path);
        let mut ranked: Vec<usize> = nodes.clone();
        ranked.sort_by(|&a, &b| {
            in_deg[b]
                .cmp(&in_deg[a])
                .then_with(|| node_name(g, a).cmp(node_name(g, b)))
        });
        for &i in ranked.iter().take(top_k) {
            let deg = in_deg[i];
            println!(
                "- {} `{}` (in_deg={})",
                kind_str(g, i),
                node_name(g, i),
                deg
            );
        }
        let extra = ranked.len().saturating_sub(top_k);
        if extra > 0 {
            println!("- _… +{extra} more_");
        }
        println!();
    }
}

/// C: 依 community 聚合，每群列代表檔/符號
fn render_c(g: &ArchivedZeroCopyGraph, in_deg: &[u32], top_k: usize) {
    println!("# Project Summary (C: community-grouped overview)\n");

    let mut by_comm: BTreeMap<u16, Vec<usize>> = BTreeMap::new();
    for (i, n) in g.nodes.iter().enumerate() {
        by_comm
            .entry(n.community_id.to_native())
            .or_default()
            .push(i);
    }
    println!(
        "Total: {} communities, {} symbols\n",
        by_comm.len(),
        g.nodes.len()
    );

    // 按 community 大小降序排序
    let mut sorted_comms: Vec<(u16, Vec<usize>)> = by_comm.into_iter().collect();
    sorted_comms.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    for (cid, nodes) in sorted_comms {
        let label = if cid == 0 {
            "Unassigned".to_string()
        } else {
            format!("Community {cid}")
        };
        // 找出此 community 的代表檔（按出現頻率）
        let mut file_count: BTreeMap<u32, usize> = BTreeMap::new();
        for &i in &nodes {
            *file_count
                .entry(g.nodes[i].file_idx.to_native())
                .or_default() += 1;
        }
        let mut files_sorted: Vec<(u32, usize)> = file_count.into_iter().collect();
        files_sorted.sort_by(|a, b| b.1.cmp(&a.1));
        let top_files: Vec<String> = files_sorted
            .iter()
            .take(top_k)
            .map(|(fi, _)| file_path(g, *fi))
            .collect();

        // 找出此 community 的代表 symbols（按 in-degree）
        let mut ranked: Vec<usize> = nodes.clone();
        ranked.sort_by(|&a, &b| in_deg[b].cmp(&in_deg[a]));
        let top_syms: Vec<String> = ranked
            .iter()
            .take(top_k)
            .map(|&i| format!("{} `{}`", kind_str(g, i), node_name(g, i)))
            .collect();

        println!(
            "## {label} ({} symbols, {} files)",
            nodes.len(),
            files_sorted.len()
        );
        println!("**Top files**: {}", top_files.join(", "));
        println!("**Top symbols**: {}", top_syms.join(", "));
        println!();
    }
}
