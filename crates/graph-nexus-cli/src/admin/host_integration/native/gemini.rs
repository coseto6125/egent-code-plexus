//! Stub leaf — Gemini CLI native integration (TypeScript BaseTool, subprocess path).
//! Real implementation lands in subproject E.

use crate::admin::status::HostStatus;
use dialoguer::theme::ColorfulTheme;

pub fn install(_theme: &ColorfulTheme) {
    println!("Gemini CLI native install — coming in subproject E.");
}

pub fn uninstall(_theme: &ColorfulTheme) {
    println!("Gemini CLI native uninstall — coming in subproject E.");
}

pub fn status() -> HostStatus {
    HostStatus::Missing
}
