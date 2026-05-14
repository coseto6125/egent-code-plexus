//! `gnx list` — enumerate repos in the registry.
//!
//! Three output formats:
//! * `compact` (default) — terminal-friendly, one repo per line
//! * `json` — structured, for scripts and parsers
//! * `toon` — minimal text optimised for LLM context windows

use clap::{Args, ValueEnum};
use gnx_core::registry::{Registry, RegistryFile, RepoEntry};

#[derive(Args, Debug, Clone)]
pub struct ListArgs {
    #[arg(long, value_enum, default_value_t = ListFormat::Compact)]
    pub format: ListFormat,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum ListFormat {
    Compact,
    Json,
    Toon,
}

pub fn run(args: ListArgs) -> Result<(), gnx_core::GnxError> {
    let home_gnx = gnx_core::registry::resolve_home_gnx();
    let registry = Registry::open(&home_gnx).map_err(|e| {
        gnx_core::GnxError::InvalidArgument(format!("registry open: {e}"))
    })?;
    let snapshot = registry.snapshot();

    let out = match args.format {
        ListFormat::Compact => render_compact(snapshot, &home_gnx.display().to_string()),
        ListFormat::Json => render_json(snapshot, &home_gnx.display().to_string())?,
        ListFormat::Toon => render_toon(snapshot),
    };
    println!("{out}");
    Ok(())
}

fn render_compact(reg: &RegistryFile, registry_path: &str) -> String {
    if reg.repos.is_empty() {
        return format!("(no repos indexed)\nregistry: {registry_path}");
    }
    let name_w = reg.repos.iter().map(|r| r.name.len()).max().unwrap_or(0).max(4);
    let mut lines = Vec::with_capacity(reg.repos.len() + 2);
    let mut total_branches = 0usize;
    for r in &reg.repos {
        total_branches += r.branches.len();
        let group = r.group.as_deref().map(|g| format!("(group: {g})")).unwrap_or_default();
        let last = latest_indexed_at(r).unwrap_or("-");
        let count = r.branches.len();
        let unit = if count == 1 { "branch" } else { "branches" };
        lines.push(format!(
            "{name:<name_w$}  {group:<20}  {count} {unit}  last: {last}",
            name = r.name,
            name_w = name_w,
        ));
    }
    lines.push(String::new());
    lines.push(format!(
        "{n} repo{plural}, {b} branches (registry: {registry_path})",
        n = reg.repos.len(),
        plural = if reg.repos.len() == 1 { "" } else { "s" },
        b = total_branches,
    ));
    lines.join("\n")
}

fn render_json(reg: &RegistryFile, registry_path: &str) -> Result<String, gnx_core::GnxError> {
    let value = serde_json::json!({
        "registry": registry_path,
        "version": reg.version,
        "repos": reg.repos,
        "groups": reg.groups,
    });
    serde_json::to_string_pretty(&value)
        .map_err(|e| gnx_core::GnxError::InvalidArgument(format!("json: {e}")))
}

fn render_toon(reg: &RegistryFile) -> String {
    let mut lines = Vec::with_capacity(reg.repos.len() + reg.groups.len() + 2);
    lines.push("repos:".to_string());
    if reg.repos.is_empty() {
        lines.push("  (none)".to_string());
    } else {
        for r in &reg.repos {
            let group = r.group.as_deref().map(|g| format!(" @{g}")).unwrap_or_default();
            let branches: Vec<String> = r
                .branches
                .iter()
                .map(|b| format!("{}:{}n", b.name, b.node_count))
                .collect();
            lines.push(format!("  - {name}{group} [{br}]", name = r.name, br = branches.join(",")));
        }
    }
    if !reg.groups.is_empty() {
        lines.push("groups:".to_string());
        for g in &reg.groups {
            lines.push(format!("  - {n}[{m}]", n = g.name, m = g.members.join(",")));
        }
    }
    lines.join("\n")
}

fn latest_indexed_at(repo: &RepoEntry) -> Option<&str> {
    repo.branches.iter().map(|b| b.indexed_at.as_str()).max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gnx_core::registry::{BranchEntry, GroupEntry, RegistryFile, RepoEntry};

    fn sample() -> RegistryFile {
        RegistryFile {
            version: 1,
            repos: vec![
                RepoEntry {
                    name: "neptune".into(),
                    remote_url: "git@x:y/neptune.git".into(),
                    worktree_path: "/w/neptune".into(),
                    index_dir_root: "/h/.gnx/neptune".into(),
                    branches: vec![BranchEntry {
                        name: "main".into(),
                        index_dir: "/h/.gnx/neptune/main".into(),
                        indexed_at: "2026-05-14T10:00".into(),
                        node_count: 1200,
                        delta_size: 0,
                        embedding_status: "complete".into(),
                    }],
                    group: Some("search".into()),
                },
                RepoEntry {
                    name: "agent".into(),
                    remote_url: "git@x:y/agent.git".into(),
                    worktree_path: "/w/agent".into(),
                    index_dir_root: "/h/.gnx/agent".into(),
                    branches: vec![BranchEntry {
                        name: "feat__x".into(),
                        index_dir: "/h/.gnx/agent/feat__x".into(),
                        indexed_at: "2026-05-13T08:00".into(),
                        node_count: 88,
                        delta_size: 0,
                        embedding_status: "skipped".into(),
                    }],
                    group: None,
                },
            ],
            groups: vec![GroupEntry { name: "search".into(), members: vec!["neptune".into()] }],
        }
    }

    #[test]
    fn render_compact_shows_repos_and_summary() {
        let s = render_compact(&sample(), "/home/x/.gnx");
        assert!(s.contains("neptune"));
        assert!(s.contains("(group: search)"));
        assert!(s.contains("agent"));
        assert!(s.contains("2026-05-14T10:00"));
        assert!(s.contains("2 repos, 2 branches"));
        assert!(s.contains("/home/x/.gnx"));
    }

    #[test]
    fn render_compact_handles_empty_registry() {
        let s = render_compact(&RegistryFile::empty(), "/tmp/x");
        assert!(s.contains("(no repos indexed)"));
        assert!(s.contains("/tmp/x"));
    }

    #[test]
    fn render_json_contains_registry_path_and_repos() {
        let s = render_json(&sample(), "/home/x/.gnx").unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["registry"], "/home/x/.gnx");
        assert_eq!(v["repos"].as_array().unwrap().len(), 2);
        assert_eq!(v["repos"][0]["name"], "neptune");
    }

    #[test]
    fn render_toon_compact_with_group_and_node_counts() {
        let s = render_toon(&sample());
        assert!(s.contains("- neptune @search"));
        assert!(s.contains("main:1200n"));
        assert!(s.contains("- agent"));
        assert!(s.contains("feat__x:88n"));
        assert!(s.contains("groups:"));
        assert!(s.contains("search[neptune]"));
    }
}
