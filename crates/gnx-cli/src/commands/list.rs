//! `gnx list` — enumerate repos in the registry.
//!
//! Routes through [`crate::output::emit`] for consistency with every other
//! command. User-facing formats: `text` (default-style human output), `json`,
//! `toon` (default; etoon-encoded for LLM context). The TOON / JSON payload
//! exposes the full `RegistryFile` fields (version, repos[], groups[]). The
//! text-format human lines include node counts in the form `<branch>:<n>n`
//! where `n` is the branch's `node_count`.

use crate::output::{emit, OutputFormat};
use clap::Args;
use gnx_core::registry::{Registry, RegistryFile, RepoEntry};
use unicode_width::UnicodeWidthStr;

#[derive(Args, Debug, Clone)]
pub struct ListArgs {
    /// Output format: text | json | toon (default: toon)
    #[arg(long, default_value = "toon")]
    pub format: Option<String>,
}

pub fn run(args: ListArgs) -> Result<(), gnx_core::GnxError> {
    let home_gnx = gnx_core::registry::resolve_home_gnx();
    let registry = Registry::open(&home_gnx)
        .map_err(|e| gnx_core::GnxError::InvalidArgument(format!("registry open: {e}")))?;
    let registry_path = home_gnx.display().to_string();
    let value = build_value(registry.snapshot(), &registry_path);
    emit(&value, OutputFormat::parse(args.format.as_deref()))
}

fn build_value(reg: &RegistryFile, registry_path: &str) -> serde_json::Value {
    serde_json::json!({
        "registry": registry_path,
        "version": reg.version,
        "repos": reg.repos,
        "groups": reg.groups,
        "results": text_lines(reg, registry_path),
    })
}

fn text_lines(reg: &RegistryFile, registry_path: &str) -> Vec<String> {
    if reg.repos.is_empty() {
        return vec![
            "(no repos indexed)".into(),
            format!("registry: {registry_path}"),
        ];
    }
    // East Asian Width: CJK chars occupy 2 terminal columns. `str::len()`
    // returns UTF-8 bytes; `{:<N}` pads to char count. Both misalign on CJK.
    let name_w = reg
        .repos
        .iter()
        .map(|r| display_width(&r.name))
        .max()
        .unwrap_or(0)
        .max(4);
    let group_w = 20usize;
    let mut lines = Vec::with_capacity(reg.repos.len() + 2);
    let mut total_branches = 0usize;
    for r in &reg.repos {
        total_branches += r.branches.len();
        let group = r
            .group
            .as_deref()
            .map(|g| format!("(group: {g})"))
            .unwrap_or_default();
        let last = latest_indexed_at(r).unwrap_or("-");
        let count = r.branches.len();
        let unit = if count == 1 { "branch" } else { "branches" };
        lines.push(format!(
            "{name}{name_pad}  {group}{group_pad}  {count} {unit}  last: {last}",
            name = r.name,
            name_pad = pad(&r.name, name_w),
            group_pad = pad(&group, group_w),
        ));
    }
    lines.push(String::new());
    lines.push(format!(
        "{n} repo{plural}, {b} branches (registry: {registry_path})",
        n = reg.repos.len(),
        plural = if reg.repos.len() == 1 { "" } else { "s" },
        b = total_branches,
    ));
    lines
}

fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn pad(s: &str, width: usize) -> String {
    " ".repeat(width.saturating_sub(display_width(s)))
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
            groups: vec![GroupEntry {
                name: "search".into(),
                members: vec!["neptune".into()],
            }],
        }
    }

    #[test]
    fn text_lines_summary_includes_repos_groups_and_totals() {
        let lines = text_lines(&sample(), "/home/x/.gnx");
        let joined = lines.join("\n");
        assert!(joined.contains("neptune"));
        assert!(joined.contains("(group: search)"));
        assert!(joined.contains("agent"));
        assert!(joined.contains("2026-05-14T10:00"));
        assert!(joined.contains("2 repos, 2 branches"));
        assert!(joined.contains("/home/x/.gnx"));
    }

    #[test]
    fn text_lines_empty_registry_shows_message_and_path() {
        let lines = text_lines(&RegistryFile::empty(), "/tmp/x");
        let joined = lines.join("\n");
        assert!(joined.contains("(no repos indexed)"));
        assert!(joined.contains("/tmp/x"));
    }

    #[test]
    fn text_lines_align_by_display_width_for_cjk_names() {
        let mut reg = sample();
        reg.repos[0].name = "搜尋系統".into(); // 4 chars, 8 columns
        reg.repos[1].name = "agent".into(); // 5 chars, 5 columns
        let lines = text_lines(&reg, "/h");
        // name_w should be 8 (cols of 搜尋系統), so `agent` row gets 3 trailing
        // spaces before the "  (group:..." separator. Total leading segment
        // for both rows must reach 8 visible columns before the gap.
        let row_cjk = lines.iter().find(|l| l.contains("搜尋系統")).unwrap();
        let row_ascii = lines.iter().find(|l| l.starts_with("agent")).unwrap();
        assert!(row_cjk.starts_with("搜尋系統  "), "cjk row: {row_cjk:?}");
        assert!(
            row_ascii.starts_with("agent     "),
            "ascii row: {row_ascii:?}"
        );
    }

    #[test]
    fn build_value_carries_registry_repos_and_text_results() {
        let v = build_value(&sample(), "/home/x/.gnx");
        assert_eq!(v["registry"], "/home/x/.gnx");
        assert_eq!(v["repos"].as_array().unwrap().len(), 2);
        assert_eq!(v["repos"][0]["name"], "neptune");
        assert!(v["groups"].as_array().unwrap().len() == 1);
        let results = v["results"].as_array().unwrap();
        assert!(!results.is_empty());
        assert!(results
            .iter()
            .any(|r| r.as_str().unwrap().contains("neptune")));
    }
}
