//! Stub leaf — Codex CLI native integration (Rust workspace dep, zero-IPC path).
//! Real implementation lands in subproject D.

use crate::admin::status::HostStatus;
use dialoguer::theme::ColorfulTheme;

pub fn install(_theme: &ColorfulTheme) {
    println!("Codex CLI native install — coming in subproject D.");
}

pub fn uninstall(_theme: &ColorfulTheme) {
    println!("Codex CLI native uninstall — coming in subproject D.");
}

pub fn status() -> HostStatus {
    HostStatus::Missing
}
