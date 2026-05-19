//! Diagnostic reports for `cgn admin`.

use crate::admin::menu::{self, select};
use cgn_core::registry::{resolve_home_cgn, RegistryFile};
use cgn_core::CgnError;
use std::path::{Path, PathBuf};
use std::process::Command;

const MENU: &[menu::Item<'_>] = &[
    ("Doctor", "run env + registry health checks together"),
    ("MCP tool list", "show the MCP tools `cgn mcp serve` exposes"),
    ("Registry health", "check index dirs, graphs, meta, orphans"),
    (
        "Environment report",
        "cgn version, paths, $HOME / $CGN_HOME, host CLIs",
    ),
    ("← Back", ""),
];

pub fn run(theme: &dialoguer::theme::ColorfulTheme) -> Result<(), CgnError> {
    loop {
        let choice = select(theme, "Diagnostics", MENU)?;
        match choice {
            Some(0) => doctor()?,
            Some(1) => mcp_tool_list()?,
            Some(2) => registry_health_report(&resolve_home_cgn())?,
            Some(3) => environment_report()?,
            Some(4) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn doctor() -> Result<(), CgnError> {
    environment_report()?;
    println!();
    registry_health_report(&resolve_home_cgn())
}

fn mcp_tool_list() -> Result<(), CgnError> {
    let exe = std::env::current_exe().map_err(|e| CgnError::Output(format!("current_exe: {e}")))?;
    let output = Command::new(exe)
        .args(["mcp", "tools"])
        .output()
        .map_err(|e| CgnError::Output(format!("cgn mcp tools: {e}")))?;
    if output.status.success() {
        print!("{}", String::from_utf8_lossy(&output.stdout));
        Ok(())
    } else {
        Err(CgnError::Output(format!(
            "cgn mcp tools: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn environment_report() -> Result<(), CgnError> {
    let exe = std::env::current_exe().map_err(|e| CgnError::Output(format!("current_exe: {e}")))?;
    println!("Environment report");
    println!("  cgn version: {}", env!("CARGO_PKG_VERSION"));
    println!("  binary: {}", exe.display());
    println!("  os: {}", std::env::consts::OS);
    println!("  arch: {}", std::env::consts::ARCH);
    println!("  cwd: {}", current_dir_display());
    for key in ["HOME", "CGN_HOME", "XDG_CONFIG_HOME", "CODEX_HOME"] {
        println!("  {key}: {}", env_value(key));
    }
    println!("  git: {}", command_version("git", &["--version"]));
    println!("  claude: {}", command_version("claude", &["--version"]));
    println!("  codex: {}", command_version("codex", &["--version"]));
    println!("  gemini: {}", command_version("gemini", &["--version"]));
    println!("  mcp command: {} mcp serve", exe.display());
    Ok(())
}

fn registry_health_report(home_cgn: &Path) -> Result<(), CgnError> {
    let health = registry_health(home_cgn)?;
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

fn registry_health(home_cgn: &Path) -> Result<RegistryHealth, CgnError> {
    let registry_path = home_cgn.join("registry.json");
    let registry = RegistryFile::read_or_empty(&registry_path)
        .map_err(|e| CgnError::InvalidArgument(format!("registry read: {e}")))?;
    // v2: repos is BTreeMap<dir_name, RepoAlias>; commit indexes live under
    // <home_cgn>/<dir_name>/commits/<commit_dirname>/graph.bin
    let mut health = RegistryHealth {
        root: home_cgn.to_path_buf(),
        registry_path,
        root_exists: home_cgn.exists(),
        registry_exists: home_cgn.join("registry.json").exists(),
        repo_count: registry.repos.len(),
        branch_count: 0, // v2 has no per-branch counter; commit count varies per repo
        ..RegistryHealth::default()
    };

    // Build set of repo dir_names that ARE registered.
    let registered_dirs: std::collections::BTreeSet<String> =
        registry.repos.keys().cloned().collect();

    // Check each registered repo's commits dir for missing graph.bin / meta.
    for (dir_name, _alias) in &registry.repos {
        let commits_dir = home_cgn.join(dir_name).join("commits");
        if let Ok(entries) = std::fs::read_dir(&commits_dir) {
            for entry in entries.flatten().filter(|e| e.path().is_dir()) {
                let index_dir = entry.path();
                let graph = index_dir.join("graph.bin");
                if !graph.is_file() {
                    health.missing_graphs.push(graph);
                }
                let meta = index_dir.join("meta.json");
                if !meta.is_file() {
                    health.missing_meta.push(meta);
                } else {
                    // Validate meta is parseable JSON
                    if std::fs::read(&meta)
                        .ok()
                        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                        .is_none()
                    {
                        health.corrupt_meta.push(meta);
                    }
                }
            }
        }
    }

    // Orphans: top-level dirs under home_cgn whose name is NOT in the registry.
    if let Ok(repos) = std::fs::read_dir(home_cgn) {
        for repo_entry in repos.flatten().filter(|entry| entry.path().is_dir()) {
            let repo_path = repo_entry.path();
            let dir_name = match repo_path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if dir_name.starts_with('_') || dir_name.starts_with('.') {
                continue;
            }
            if registered_dirs.contains(&dir_name) {
                continue;
            }
            // This dir is not in registry → any commit dirs with graph.bin are orphans.
            let commits_dir = repo_path.join("commits");
            if let Ok(commits) = std::fs::read_dir(&commits_dir) {
                for commit_entry in commits.flatten().filter(|e| e.path().is_dir()) {
                    let path = commit_entry.path();
                    if path.join("graph.bin").exists() {
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
    use cgn_core::registry::RepoAlias;
    use std::collections::BTreeMap;

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
    fn registry_health_reports_orphan_indexes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        // v2 layout: commits live under <home>/<dir_name>/commits/<commit_dirname>/
        let registered_dir_name = "repo__aabbccdd";
        // orphan: a top-level dir NOT in the registry, with a commit subdir containing graph.bin
        let orphan_dir_name = "ghost__deadbeef";
        let orphan_commit = home
            .join(orphan_dir_name)
            .join("commits")
            .join("sha_orphan9");
        std::fs::create_dir_all(&orphan_commit).expect("orphan dir");
        std::fs::write(orphan_commit.join("graph.bin"), b"graph").expect("orphan graph");

        let mut repos = BTreeMap::new();
        repos.insert(
            registered_dir_name.into(),
            RepoAlias {
                dir_name: registered_dir_name.into(),
                common_dir: "/work/repo/.git".into(),
                remote_url: Some("https://example.test/repo.git".into()),
                aliases: vec!["repo".into()],
                last_touched: "2026-05-16T00:00:00Z".into(),
                groups: vec![],
            },
        );
        RegistryFile::write_atomic(
            &home.join("registry.json"),
            &RegistryFile {
                version: 2,
                repos,
                groups: vec![],
            },
        )
        .expect("write registry");

        let health = registry_health(home).expect("health");

        assert_eq!(health.repo_count, 1);
        assert_eq!(health.branch_count, 0);
        // orphan_commit is under a dir NOT in registry → it's an orphan
        assert_eq!(health.orphan_index_dirs, vec![orphan_commit]);
        assert!(health.missing_index_dirs.is_empty());
    }
}
