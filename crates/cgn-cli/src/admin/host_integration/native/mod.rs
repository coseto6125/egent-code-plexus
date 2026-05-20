//! Native sub-menu — pick non-Codex hosts for zero-IPC / fork integration.

pub mod codex;
pub mod gemini;

use crate::admin::menu::{self, select};
use crate::admin::status::HostStatus;
use cgn_core::CgnError;
use dialoguer::theme::ColorfulTheme;

const HOSTS: &[menu::Item<'_>] = &[
    ("Gemini CLI", "register cgn as a native tool in Gemini CLI"),
    ("← Back", ""),
];

const ACTIONS: &[menu::Item<'_>] = &[
    ("install", "write the native tool registration"),
    ("uninstall", "remove the native tool registration"),
    ("status", "show whether the native tool is registered"),
    ("← Back", ""),
];

/// Entry point called from `host_integration::run`.
pub fn run(theme: &ColorfulTheme) -> Result<(), CgnError> {
    loop {
        let choice = select(theme, "Native — pick a host", HOSTS)?;
        match choice {
            Some(0) => host_menu(
                theme,
                "Gemini CLI",
                gemini::install,
                gemini::uninstall,
                gemini::status,
            )?,
            Some(1) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn host_menu(
    theme: &ColorfulTheme,
    host_name: &str,
    install: fn(&ColorfulTheme),
    uninstall: fn(&ColorfulTheme),
    status: fn() -> HostStatus,
) -> Result<(), CgnError> {
    loop {
        let choice = select(theme, &format!("{host_name} — action"), ACTIONS)?;
        match choice {
            Some(0) => install(theme),
            Some(1) => uninstall(theme),
            Some(2) => status().print(host_name),
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}
