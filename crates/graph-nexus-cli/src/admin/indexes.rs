//! Index maintenance workflows for `gnx admin`.

use crate::admin::menu::{self, select};
use crate::commands::admin::{drop, index, prune};
use dialoguer::{theme::ColorfulTheme, Confirm, Input};
use graph_nexus_core::registry::{resolve_home_gnx, Registry, RegistryFile};
use graph_nexus_core::GnxError;
use std::path::PathBuf;

const MENU: &[menu::Item<'_>] = &[
    ("Build / refresh index", "(re)scan a repo and write graph.bin"),
    ("Inspect indexed repos", "list every repo + branch in the registry"),
    ("Prune stale indexes", "delete one branch or all orphan index dirs"),
    ("Drop index", "remove a repo's index data and registry entry"),
    ("← Back", ""),
];

pub fn run(theme: &ColorfulTheme) -> Result<(), GnxError> {
    loop {
        let choice = select(theme, "Indexes", MENU)?;
        match choice {
            Some(0) => build_refresh_wizard(theme)?,
            Some(1) => inspect_indexed_repos()?,
            Some(2) => prune_wizard(theme)?,
            Some(3) => drop_wizard(theme)?,
            Some(4) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn build_refresh_wizard(theme: &ColorfulTheme) -> Result<(), GnxError> {
    let repo = input(theme, "Repo path", ".")?;
    let force = Confirm::with_theme(theme)
        .with_prompt("Force full refresh")
        .default(false)
        .interact()
        .map_err(dialoguer_err)?;

    index::run(index::IndexArgs {
        repo,
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
    for (dir_name, alias) in &registry.repos {
        let display_name = alias.aliases.first().map(|s| s.as_str()).unwrap_or(dir_name);
        println!("  {}", display_name);
        println!("    dir_name: {}", dir_name);
        println!("    common_dir: {}", alias.common_dir);
        if let Some(url) = &alias.remote_url {
            println!("    remote: {}", url);
        }
        if alias.groups.is_empty() {
            println!("    groups: -");
        } else {
            println!("    groups: {}", alias.groups.join(", "));
        }
    }
}

fn prune_wizard(theme: &ColorfulTheme) -> Result<(), GnxError> {
    let confirmed = Confirm::with_theme(theme)
        .with_prompt("Sweep orphan repos (common_dir no longer exists)")
        .default(false)
        .interact()
        .map_err(dialoguer_err)?;
    if confirmed {
        prune::run(prune::PruneArgs {
            orphans: true,
            branch: None,
            repo: None,
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
    use graph_nexus_core::registry::RepoAlias;
    use std::collections::BTreeMap;

    #[test]
    fn indexes_menu_matches_target_order() {
        let labels: Vec<&str> = MENU.iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            vec![
                "Build / refresh index",
                "Inspect indexed repos",
                "Prune stale indexes",
                "Drop index",
                "← Back",
            ]
        );
    }

    #[test]
    fn print_registry_accepts_empty_and_populated_registry() {
        print_registry(&RegistryFile::empty(), std::path::Path::new("/tmp/gnx"));
        let mut repos = BTreeMap::new();
        repos.insert(
            "repo__aabbccdd".into(),
            RepoAlias {
                dir_name: "repo__aabbccdd".into(),
                common_dir: "/work/repo/.git".into(),
                remote_url: Some("https://example.test/repo.git".into()),
                aliases: vec!["repo".into()],
                last_touched: "2026-05-16T00:00:00Z".into(),
                groups: vec!["core".into()],
            },
        );
        let registry = RegistryFile {
            version: 2,
            repos,
            groups: vec![],
        };
        print_registry(&registry, std::path::Path::new("/tmp/gnx"));
    }
}
