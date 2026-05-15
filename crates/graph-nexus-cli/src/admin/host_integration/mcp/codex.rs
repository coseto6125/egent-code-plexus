//! Stub leaf — Codex CLI MCP integration (no-fork side-car path).
//! Real implementation lands in subproject C.

use crate::admin::status::HostStatus;
use dialoguer::theme::ColorfulTheme;

pub fn install(_theme: &ColorfulTheme) {
    println!("Codex CLI MCP install — coming in subproject C.");
}

pub fn uninstall(_theme: &ColorfulTheme) {
    println!("Codex CLI MCP uninstall — coming in subproject C.");
}

pub fn status() -> HostStatus {
    HostStatus::Missing
}
