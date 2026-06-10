//! `ecp peers plan` — pre-spawn disjointness query.
//!
//! A team lead about to split work across agents asks: "can these targets be
//! edited in parallel without colliding?" Each target's blast radius is
//! IMPACT(target, both directions); two targets that share any impacted node
//! belong in the same work package. Detecting the collision before spawn is
//! the proactive complement to the watcher's reactive HARD/SOFT concerns.
//!
//! Engine cost is paid only by this subcommand: the rest of the peers family
//! stays on main's no-graph fast path, so `status`/`say`/watch keep their
//! current latency.

use crate::commands::impact::run_for_symbol;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

const CAVEAT: &str = "impact caller sets are a lower bound (ambiguous bare calls to common names \
     are suppressed) — for very common symbol names, grep before trusting zero overlap";

#[derive(Debug)]
struct TargetImpact {
    name: String,
    uids: HashSet<String>,
    uid_names: HashMap<String, String>,
    count: usize,
}

pub fn cmd_plan(
    targets: &[String],
    direction: &str,
    depth: u32,
    include_tests: bool,
    json: bool,
) -> io::Result<()> {
    let cwd = std::env::current_dir()?;
    let graph_path = crate::graph_path::resolve(Path::new(".ecp/graph.bin"), &cwd);
    let engine = crate::auto_ensure::load_ensured(&graph_path, &cwd).map_err(|e| {
        io::Error::other(format!(
            "no usable graph for {} ({e}) — run `ecp admin index --repo .` first",
            cwd.display()
        ))
    })?;
    let member_repo = crate::repo_identity::repo_dir_name_for_cwd(&cwd)?;

    let mut resolved: Vec<TargetImpact> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    for t in targets {
        let local = match run_for_symbol(
            &engine,
            &member_repo,
            t,
            direction,
            Some(depth),
            None,
            include_tests,
        ) {
            Ok(l) => l,
            // Missing symbol is an expected per-target outcome here, not a
            // command failure: the lead asks about a batch and needs the
            // honest miss-list alongside the resolved ones.
            Err(e) if e.to_string().contains("No symbol named") => {
                unresolved.push(t.clone());
                continue;
            }
            Err(e) => return Err(io::Error::other(e.to_string())),
        };
        let payload = local.as_json();
        if payload.get("error").is_some() {
            unresolved.push(t.clone());
            continue;
        }
        let mut uids = HashSet::new();
        let mut uid_names = HashMap::new();
        if let Some(items) = payload["impact"].as_array() {
            for item in items {
                if let Some(uid) = item["uid"].as_str() {
                    uids.insert(uid.to_string());
                    if let Some(n) = item["name"].as_str() {
                        uid_names.insert(uid.to_string(), n.to_string());
                    }
                }
            }
        }
        let count = uids.len();
        resolved.push(TargetImpact {
            name: t.clone(),
            uids,
            uid_names,
            count,
        });
    }

    let (overlaps, clusters) = analyze(&resolved);
    render(&resolved, &unresolved, &overlaps, &clusters, json)
}

struct Overlap {
    a: String,
    b: String,
    shared_count: usize,
    sample: Vec<String>,
}

/// Pairwise intersections + union-find merge into disjoint work packages.
fn analyze(targets: &[TargetImpact]) -> (Vec<Overlap>, Vec<Vec<String>>) {
    let n = targets.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut Vec<usize>, i: usize) -> usize {
        if parent[i] != i {
            let root = find(parent, parent[i]);
            parent[i] = root;
        }
        parent[i]
    }
    let mut overlaps = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            let shared: Vec<&String> = targets[i].uids.intersection(&targets[j].uids).collect();
            if shared.is_empty() {
                continue;
            }
            let mut sample: Vec<String> = shared
                .iter()
                .take(3)
                .map(|uid| {
                    targets[i]
                        .uid_names
                        .get(*uid)
                        .or_else(|| targets[j].uid_names.get(*uid))
                        .unwrap_or(uid)
                        .clone()
                })
                .collect();
            sample.sort();
            overlaps.push(Overlap {
                a: targets[i].name.clone(),
                b: targets[j].name.clone(),
                shared_count: shared.len(),
                sample,
            });
            let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
            if ri != rj {
                parent[ri] = rj;
            }
        }
    }
    let mut groups: HashMap<usize, Vec<String>> = HashMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        groups
            .entry(root)
            .or_default()
            .push(targets[i].name.clone());
    }
    let mut clusters: Vec<Vec<String>> = groups.into_values().collect();
    clusters.sort();
    (overlaps, clusters)
}

fn render(
    targets: &[TargetImpact],
    unresolved: &[String],
    overlaps: &[Overlap],
    clusters: &[Vec<String>],
    json: bool,
) -> io::Result<()> {
    if json {
        let payload = serde_json::json!({
            "targets": targets.iter().map(|t| serde_json::json!({
                "name": t.name,
                "impact_count": t.count,
            })).collect::<Vec<_>>(),
            "unresolved": unresolved,
            "overlaps": overlaps.iter().map(|o| serde_json::json!({
                "a": o.a,
                "b": o.b,
                "shared_count": o.shared_count,
                "sample": o.sample,
            })).collect::<Vec<_>>(),
            "clusters": clusters,
            "caveat": CAVEAT,
        });
        println!(
            "{}",
            serde_json::to_string(&payload).map_err(io::Error::other)?
        );
        return Ok(());
    }
    println!(
        "plan: {} target{}, {} unresolved",
        targets.len(),
        if targets.len() == 1 { "" } else { "s" },
        unresolved.len()
    );
    for (i, c) in clusters.iter().enumerate() {
        println!("cluster {}: {}", i + 1, c.join(", "));
    }
    if overlaps.is_empty() {
        println!("no overlaps — targets are pairwise disjoint at the analyzed depth");
    } else {
        for o in overlaps {
            println!(
                "overlap: {} ↔ {} share {} symbol{} (e.g. {})",
                o.a,
                o.b,
                o.shared_count,
                if o.shared_count == 1 { "" } else { "s" },
                o.sample.join(", ")
            );
        }
    }
    if !unresolved.is_empty() {
        println!("unresolved: {}", unresolved.join(", "));
    }
    println!("caveat: {CAVEAT}");
    Ok(())
}
