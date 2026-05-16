//! Diagnostic reports for `gnx admin`.

use crate::admin::menu::{self, select};
use graph_nexus_core::registry::{resolve_home_gnx, BranchMeta, RegistryFile};
use graph_nexus_core::GnxError;
use std::path::{Path, PathBuf};
use std::process::Command;

const MENU: &[menu::Item<'_>] = &[
    ("Doctor", "run env + registry health checks together"),
    ("MCP tool list", "show the MCP tools `gnx mcp serve` exposes"),
    ("Registry health", "check index dirs, graphs, meta, orphans"),
    (
        "Environment report",
        "gnx version, paths, $HOME / $GNX_HOME, host CLIs",
    ),
    ("← Back", ""),
];

pub fn run(theme: &dialoguer::theme::ColorfulTheme) -> Result<(), GnxError> {
    loop {
        let choice = select(theme, "Diagnostics", MENU)?;
        match choice {
            Some(0) => doctor()?,
            Some(1) => mcp_tool_list()?,
            Some(2) => registry_health_report(&resolve_home_gnx())?,
            Some(3) => environment_report()?,
            Some(4) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn doctor() -> Result<(), GnxError> {
    environment_report()?;
    println!();
    registry_health_report(&resolve_home_gnx())
}

fn mcp_tool_list() -> Result<(), GnxError> {
    let exe = std::env::current_exe().map_err(|e| GnxError::Output(format!("current_exe: {e}")))?;
    let output = Command::new(exe)
        .args(["mcp", "tools"])
        .output()
        .map_err(|e| GnxError::Output(format!("gnx mcp tools: {e}")))?;
    if output.status.success() {
        print!("{}", String::from_utf8_lossy(&output.stdout));
        Ok(())
    } else {
        Err(GnxError::Output(format!(
            "gnx mcp tools: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn environment_report() -> Result<(), GnxError> {
    let exe = std::env::current_exe().map_err(|e| GnxError::Output(format!("current_exe: {e}")))?;
    println!("Environment report");
    println!("  gnx version: {}", env!("CARGO_PKG_VERSION"));
    println!("  binary: {}", exe.display());
    println!("  os: {}", std::env::consts::OS);
    println!("  arch: {}", std::env::consts::ARCH);
    println!("  cwd: {}", current_dir_display());
    for key in ["HOME", "GNX_HOME", "XDG_CONFIG_HOME", "CODEX_HOME"] {
        println!("  {key}: {}", env_value(key));
    }
    println!("  git: {}", command_version("git", &["--version"]));
    println!("  claude: {}", command_version("claude", &["--version"]));
    println!("  codex: {}", command_version("codex", &["--version"]));
    println!("  gemini: {}", command_version("gemini", &["--version"]));
    println!("  mcp command: {} mcp serve", exe.display());
    Ok(())
}

fn registry_health_report(home_gnx: &Path) -> Result<(), GnxError> {
    let health = registry_health(home_gnx)?;
    println!("Registry health");
    println!("  root: {}", health.root.display());
    println!("  registry: {}", health.registry_path.display());
    println!("  root_exists: {}", health.root_exists);
    println!("  registry_exists: {}", health.registry_exists);
    println!("  repo_count: {}", health.repo_count);
    println!("  branch_count: {}", health.branch_count);
    println!("  missing_index_dirs: {}", health.missing_index_dirs.len());
    for path in &health.missing_index_dirs {
        println!("    missing index dir: {}", path.display());
    }
    println!("  missing_graphs: {}", health.missing_graphs.len());
    for path in &health.missing_graphs {
        println!("    missing graph: {}", path.display());
    }
    println!("  missing_meta: {}", health.missing_meta.len());
    for path in &health.missing_meta {
        println!("    missing meta: {}", path.display());
    }
    println!("  corrupt_meta: {}", health.corrupt_meta.len());
    for path in &health.corrupt_meta {
        println!("    corrupt meta: {}", path.display());
    }
    println!("  orphan_index_dirs: {}", health.orphan_index_dirs.len());
    for path in &health.orphan_index_dirs {
        println!("    orphan index dir: {}", path.display());
    }
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RegistryHealth {
    root: PathBuf,
    registry_path: PathBuf,
    root_exists: bool,
    registry_exists: bool,
    repo_count: usize,
    branch_count: usize,
    missing_index_dirs: Vec<PathBuf>,
    missing_graphs: Vec<PathBuf>,
    missing_meta: Vec<PathBuf>,
    corrupt_meta: Vec<PathBuf>,
    orphan_index_dirs: Vec<PathBuf>,
}

fn registry_health(home_gnx: &Path) -> Result<RegistryHealth, GnxError> {
    let registry_path = home_gnx.join("registry.json");
    let registry = RegistryFile::read_or_empty(&registry_path)
        .map_err(|e| GnxError::InvalidArgument(format!("registry read: {e}")))?;
    let mut health = RegistryHealth {
        root: home_gnx.to_path_buf(),
        registry_path,
        root_exists: home_gnx.exists(),
        registry_exists: home_gnx.join("registry.json").exists(),
        repo_count: registry.repos.len(),
        branch_count: registry.repos.iter().map(|repo| repo.branches.len()).sum(),
        ..RegistryHealth::default()
    };

    let mut expected_index_dirs = std::collections::BTreeSet::new();
    for repo in &registry.repos {
        for branch in &repo.branches {
            let index_dir = PathBuf::from(&branch.index_dir);
            expected_index_dirs.insert(index_dir.clone());
            if !index_dir.is_dir() {
                health.missing_index_dirs.push(index_dir.clone());
                continue;
            }
            let graph = index_dir.join("graph.bin");
            if !graph.is_file() {
                health.missing_graphs.push(graph);
            }
            let meta = index_dir.join("meta.json");
            if !meta.is_file() {
                health.missing_meta.push(meta);
            } else if BranchMeta::read(&meta).is_err() {
                health.corrupt_meta.push(meta);
            }
        }
    }

    if let Ok(repos) = std::fs::read_dir(home_gnx) {
        for repo_entry in repos.flatten().filter(|entry| entry.path().is_dir()) {
            let repo_path = repo_entry.path();
            if repo_path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('_'))
            {
                continue;
            }
            if let Ok(branches) = std::fs::read_dir(&repo_path) {
                for branch_entry in branches.flatten().filter(|entry| entry.path().is_dir()) {
                    let path = branch_entry.path();
                    if path.join("graph.bin").exists() && !expected_index_dirs.contains(&path) {
                        health.orphan_index_dirs.push(path);
                    }
                }
            }
        }
    }

    Ok(health)
}

fn env_value(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| "-".into())
}

fn current_dir_display() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|e| format!("unavailable: {e}"))
}

fn command_version(command: &str, args: &[&str]) -> String {
    match Command::new(command).args(args).output() {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or("ok")
            .to_string(),
        Ok(output) => format!(
            "error: {}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or("unknown")
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "not found".into(),
        Err(e) => format!("error: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use graph_nexus_core::registry::{BranchEntry, RepoEntry};

    #[test]
    fn diagnostics_menu_matches_target_order() {
        let labels: Vec<&str> = MENU.iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            vec![
                "Doctor",
                "MCP tool list",
                "Registry health",
                "Environment report",
                "← Back",
            ]
        );
    }

    #[test]
    fn registry_health_reports_missing_and_orphan_indexes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        let expected = home.join("repo").join("main");
        let orphan = home.join("repo").join("old");
        std::fs::create_dir_all(&orphan).expect("orphan dir");
        std::fs::write(orphan.join("graph.bin"), b"graph").expect("orphan graph");
        std::fs::create_dir_all(home).expect("home dir");
        RegistryFile::write_atomic(
            &home.join("registry.json"),
            &RegistryFile {
                version: 1,
                repos: vec![RepoEntry {
                    name: "repo".into(),
                    remote_url: "https://example.test/repo.git".into(),
                    worktree_path: "/work/repo".into(),
                    index_dir_root: home.join("repo").to_string_lossy().into_owned(),
                    branches: vec![BranchEntry {
                        name: "main".into(),
                        index_dir: expected.to_string_lossy().into_owned(),
                        indexed_at: "2026-05-16T00:00:00Z".into(),
                        node_count: 1,
                        delta_size: 0,
                    }],
                    groups: vec![],
                }],
                groups: vec![],
            },
        )
        .expect("write registry");

        let health = registry_health(home).expect("health");

        assert_eq!(health.repo_count, 1);
        assert_eq!(health.branch_count, 1);
        assert_eq!(health.missing_index_dirs, vec![expected]);
        assert_eq!(health.orphan_index_dirs, vec![orphan]);
    }
}
