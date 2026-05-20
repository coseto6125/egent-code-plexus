//! `cgn admin` — interactive TUI for operational maintenance.
//!
//! Opens a `dialoguer`-based menu tree.  Index, agent integration,
//! config, group, and diagnostic workflows are reachable from here.
//! Scriptable host-integration operations live under `cgn admin <subcmd>`;
//! this TUI remains the progressive human-operated path.
//!
//! # Menu tree
//! ```text
//! cgn admin
//! ├── Indexes
//! ├── Agent Integrations
//! ├── Config
//! ├── Groups
//! └── Diagnostics
//! ```

pub mod config;
pub mod diagnostics;
pub mod gc;
pub mod groups;
pub mod host_integration;
pub mod indexes;
pub mod menu;
pub mod status;

use cgn_core::CgnError;
use dialoguer::theme::ColorfulTheme;

#[derive(clap::Args, Debug, Clone)]
pub struct AdminArgs {
    // No fields — admin always opens the interactive TUI.
    // (Subproject C may add --non-interactive for scripting later.)
}

pub fn run(_args: AdminArgs) -> Result<(), CgnError> {
    let theme = ColorfulTheme::default();
    main_menu(&theme)
}

fn main_menu(theme: &ColorfulTheme) -> Result<(), CgnError> {
    loop {
        let choice = menu::select(theme, "cgn admin", MAIN_MENU)?;
        match choice {
            Some(0) => indexes::run(theme)?,
            Some(1) => host_integration::run(theme)?,
            Some(2) => config::run(theme)?,
            Some(3) => groups::run(theme)?,
            Some(4) => diagnostics::run(theme)?,
            Some(5) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

pub const MAIN_MENU: &[menu::Item<'_>] = &[
    ("Indexes", "build, inspect, prune, drop indexes"),
    ("Agent Integrations", "MCP / native / hooks for LLM hosts"),
    ("Config", "view, edit, validate cgn.toml"),
    ("Groups", "multi-repo grouping for cross-repo contracts"),
    ("Diagnostics", "doctor, registry health, env report"),
    ("Exit", ""),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_admin_menu_matches_target_information_architecture() {
        let labels: Vec<&str> = MAIN_MENU.iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            vec![
                "Indexes",
                "Agent Integrations",
                "Config",
                "Groups",
                "Diagnostics",
                "Exit",
            ]
        );
    }
}
