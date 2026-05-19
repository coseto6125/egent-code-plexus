//! Stub leaf — Cursor MCP integration.
//! Real implementation lands in subproject C.

use crate::admin::status::HostStatus;
use dialoguer::theme::ColorfulTheme;

pub fn install(_theme: &ColorfulTheme) {
    println!("Cursor MCP install — coming in subproject C.");
}

pub fn uninstall(_theme: &ColorfulTheme) {
    println!("Cursor MCP uninstall — coming in subproject C.");
}

pub fn status() -> HostStatus {
    HostStatus::Missing
}
