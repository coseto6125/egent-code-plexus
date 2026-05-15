//! `gnx admin` — interactive TUI for host integration management.
//!
//! Opens a `dialoguer`-based menu tree.  All install / uninstall / status
//! actions for MCP and Native integrations are reachable from here.
//! No top-level `gnx install` / `gnx integrate` command is exposed — this
//! is the sole entry point per the UX constraint in the host-integration spec.
//!
//! # Menu tree
//! ```text
//! gnx admin
//! └── Bind tool to code agent
//!     ├── Native (no side-car)
//!     │   ├── Codex CLI → install / uninstall / status
//!     │   └── Gemini CLI → install / uninstall / status
//!     └── MCP (shared side-car)
//!         └── (8 hosts) → install / uninstall / status
//! ```

pub mod codex_native;
pub mod host_integration;
pub mod menu;
pub mod status;

use dialoguer::theme::ColorfulTheme;
use graph_nexus_core::GnxError;

#[derive(clap::Args, Debug, Clone)]
pub struct AdminArgs {
    // No fields — admin always opens the interactive TUI.
    // (Subproject C may add --non-interactive for scripting later.)
}

pub fn run(_args: AdminArgs) -> Result<(), GnxError> {
    let theme = ColorfulTheme::default();
    main_menu(&theme)
}

fn main_menu(theme: &ColorfulTheme) -> Result<(), GnxError> {
    loop {
        let choice = menu::select(theme, "gnx admin", &["Bind tool to code agent", "Exit"])?;
        match choice {
            Some(0) => host_integration::run(theme)?,
            Some(1) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}
