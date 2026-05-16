//! Index maintenance workflows for `gnx admin`.

use crate::admin::menu::select;
use crate::commands::admin::{drop, index, prune, rename_branch};
use dialoguer::{theme::ColorfulTheme, Confirm, Input};
use graph_nexus_core::registry::{resolve_home_gnx, Registry, RegistryFile};
use graph_nexus_core::GnxError;
use std::path::PathBuf;

const MENU: &[&str] = &[
    "Build / refresh index",
    "Inspect indexed repos",
    "Rename branch index",
    "Prune stale indexes",
    "Drop index",
    "← Back",
];

pub fn run(theme: &ColorfulTheme) -> Result<(), GnxError> {
    loop {
        let choice = select(theme, "Indexes", MENU)?;
        match choice {
            Some(0) => build_refresh_wizard(theme)?,
            Some(1) => inspect_indexed_repos()?,
            Some(2) => rename_branch_wizard(theme)?,
            Some(3) => prune_wizard(theme)?,
            Some(4) => drop_wizard(theme)?,
            Some(5) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn build_refresh_wizard(theme: &ColorfulTheme) -> Result<(), GnxError> {
    let repo = input(theme, "Repo path", ".")?;
    let embeddings = Confirm::with_theme(theme)
        .with_prompt("Build embeddings")
        .default(false)
        .interact()
        .map_err(dialoguer_err)?;
    let force = Confirm::with_theme(theme)
        .with_prompt("Force full refresh")
        .default(false)
        .interact()
        .map_err(dialoguer_err)?;

    index::run(index::IndexArgs {
        repo,
        embeddings,
        drop_embeddings: false,
        force,
        dump_resolver: None,
        no_cache: false,
        quiet: false,
    })
    .map_err(GnxError::Output)
}

fn inspect_indexed_repos() -> Result<(), GnxError> {
    let home_gnx = resolve_home_gnx();
    let registry = Registry::open(&home_gnx)
        .map_err(|e| GnxError::InvalidArgument(format!("registry open: {e}")))?;
    print_registry(registry.snapshot(), &home_gnx);
    Ok(())
}

fn print_registry(registry: &RegistryFile, home_gnx: &std::path::Path) {
    println!("Registry: {}", home_gnx.join("registry.json").display());
    if registry.repos.is_empty() {
        println!("  no indexed repos");
        return;
    }
    for repo in &registry.repos {
        println!("  {}", repo.name);
        println!("    worktree: {}", repo.worktree_path);
        println!("    remote: {}", repo.remote_url);
        if repo.groups.is_empty() {
            println!("    groups: -");
        } else {
            println!("    groups: {}", repo.groups.join(", "));
        }
        for branch in &repo.branches {
            println!(
                "    branch {:<24} nodes={} embeddings={} indexed_at={}",
                branch.name, branch.node_count, branch.embedding_status, branch.indexed_at
            );
        }
    }
}

fn rename_branch_wizard(theme: &ColorfulTheme) -> Result<(), GnxError> {
    let repo = input_path(theme, "Repo path", ".")?;
    let from = input(theme, "From branch", "")?;
    let to = input(theme, "To branch", "")?;
    rename_branch::run(rename_branch::RenameBranchArgs { from, to, repo })
}

fn prune_wizard(theme: &ColorfulTheme) -> Result<(), GnxError> {
    let repo = input_path(theme, "Repo path", ".")?;
    let branch = input(theme, "Branch to prune", "")?;
    let confirmed = Confirm::with_theme(theme)
        .with_prompt(format!("Delete index data for branch `{branch}`"))
        .default(false)
        .interact()
        .map_err(dialoguer_err)?;
    if confirmed {
        prune::run(prune::PruneArgs {
            orphans: false,
            branch: Some(branch),
            repo: Some(repo),
        })?;
    }
    Ok(())
}

fn drop_wizard(theme: &ColorfulTheme) -> Result<(), GnxError> {
    let all = Confirm::with_theme(theme)
        .with_prompt("Drop every registered repo")
        .default(false)
        .interact()
        .map_err(dialoguer_err)?;
    if all {
        let confirmed = Confirm::with_theme(theme)
            .with_prompt("This deletes all gnx index data and registry entries. Continue")
            .default(false)
            .interact()
            .map_err(dialoguer_err)?;
        if confirmed {
            drop::run(drop::DropArgs {
                repo: PathBuf::from("."),
                all: true,
            })?;
        }
        return Ok(());
    }

    let repo = input_path(theme, "Repo path", ".")?;
    let confirmed = Confirm::with_theme(theme)
        .with_prompt(format!("Drop gnx index data for {}", repo.display()))
        .default(false)
        .interact()
        .map_err(dialoguer_err)?;
    if confirmed {
        drop::run(drop::DropArgs { repo, all: false })?;
    }
    Ok(())
}

fn input(theme: &ColorfulTheme, prompt: &str, default: &str) -> Result<String, GnxError> {
    Input::with_theme(theme)
        .with_prompt(prompt)
        .default(default.to_string())
        .interact_text()
        .map_err(dialoguer_err)
}

fn input_path(theme: &ColorfulTheme, prompt: &str, default: &str) -> Result<PathBuf, GnxError> {
    input(theme, prompt, default).map(PathBuf::from)
}

fn dialoguer_err(e: dialoguer::Error) -> GnxError {
    GnxError::Output(format!("dialoguer: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use graph_nexus_core::registry::{BranchEntry, RepoEntry};

    #[test]
    fn indexes_menu_matches_target_order() {
        assert_eq!(
            MENU,
            &[
                "Build / refresh index",
                "Inspect indexed repos",
                "Rename branch index",
                "Prune stale indexes",
                "Drop index",
                "← Back",
            ]
        );
    }

    #[test]
    fn print_registry_accepts_empty_and_populated_registry() {
        print_registry(&RegistryFile::empty(), std::path::Path::new("/tmp/gnx"));
        let registry = RegistryFile {
            version: 1,
            repos: vec![RepoEntry {
                name: "repo".into(),
                remote_url: "https://example.test/repo.git".into(),
                worktree_path: "/work/repo".into(),
                index_dir_root: "/home/me/.gnx/repo".into(),
                branches: vec![BranchEntry {
                    name: "main".into(),
                    index_dir: "/home/me/.gnx/repo/main".into(),
                    indexed_at: "2026-05-16T00:00:00Z".into(),
                    node_count: 1,
                    delta_size: 0,
                    embedding_status: "none".into(),
                }],
                groups: vec!["core".into()],
            }],
            groups: vec![],
        };
        print_registry(&registry, std::path::Path::new("/tmp/gnx"));
    }
}
