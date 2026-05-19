//! Group management workflows for `cgn admin`.

use crate::admin::menu::{self, select};
use crate::commands::admin::group;
use dialoguer::{theme::ColorfulTheme, Input};
use cgn_core::registry::{resolve_home_cgn, Registry};
use cgn_core::CgnError;

const MENU: &[menu::Item<'_>] = &[
    (
        "Create / add repo",
        "attach a repo to a group (creates group if new)",
    ),
    ("Remove repo", "detach a repo from a group"),
    ("Sync contracts", "list groups for cross-repo contract sync"),
    ("← Back", ""),
];

pub fn run(theme: &ColorfulTheme) -> Result<(), CgnError> {
    loop {
        let choice = select(theme, "Groups", MENU)?;
        match choice {
            Some(0) => add_repo_wizard(theme)?,
            Some(1) => remove_repo_wizard(theme)?,
            Some(2) => sync_contracts(),
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn add_repo_wizard(theme: &ColorfulTheme) -> Result<(), CgnError> {
    let repo = input(theme, "Repo name")?;
    let group_name = input(theme, "Group name")?;
    group::run(group::GroupCommands::Add {
        repo,
        group: group_name,
    })
}

fn remove_repo_wizard(theme: &ColorfulTheme) -> Result<(), CgnError> {
    let repo = input(theme, "Repo name")?;
    let group_name = input(theme, "Group name")?;
    group::run(group::GroupCommands::Remove {
        repo,
        group: group_name,
    })
}

fn sync_contracts() {
    let home_cgn = resolve_home_cgn();
    match Registry::open(&home_cgn) {
        Ok(registry) => {
            if registry.snapshot().groups.is_empty() {
                println!("No groups to sync.");
                return;
            }
            println!("Groups registered for contract sync:");
            for group in &registry.snapshot().groups {
                println!("  {}: {}", group.name, group.members.join(", "));
            }
            println!("Contract sync is not implemented yet; use `cgn contracts` for inventory.");
        }
        Err(e) => eprintln!("Cannot open registry: {e}"),
    }
}

fn input(theme: &ColorfulTheme, prompt: &str) -> Result<String, CgnError> {
    Input::with_theme(theme)
        .with_prompt(prompt)
        .interact_text()
        .map_err(|e| CgnError::Output(format!("dialoguer: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_menu_matches_target_order() {
        let labels: Vec<&str> = MENU.iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            vec![
                "Create / add repo",
                "Remove repo",
                "Sync contracts",
                "← Back",
            ]
        );
    }
}
