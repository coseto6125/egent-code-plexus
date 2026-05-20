//! Stub leaf — Generic MCP host integration (any MCP-capable host).
//! Real implementation lands in subproject C.

use crate::admin::status::HostStatus;
use dialoguer::theme::ColorfulTheme;

pub fn install(_theme: &ColorfulTheme) {
    println!("Generic MCP host install — coming in subproject C.");
}

pub fn uninstall(_theme: &ColorfulTheme) {
    println!("Generic MCP host uninstall — coming in subproject C.");
}

pub fn status() -> HostStatus {
    HostStatus::Missing
}
