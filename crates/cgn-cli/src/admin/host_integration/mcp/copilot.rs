//! Stub leaf — GitHub Copilot MCP integration.
//! Real implementation lands in subproject C.

use crate::admin::status::HostStatus;
use dialoguer::theme::ColorfulTheme;

pub fn install(_theme: &ColorfulTheme) {
    println!("Copilot MCP install — coming in subproject C.");
}

pub fn uninstall(_theme: &ColorfulTheme) {
    println!("Copilot MCP uninstall — coming in subproject C.");
}

pub fn status() -> HostStatus {
    HostStatus::Missing
}
