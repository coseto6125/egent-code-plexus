//! Gemini CLI MCP integration.

use crate::admin::host_integration::gemini_cli;
use crate::admin::status::HostStatus;
use cgn_core::CgnError;
use dialoguer::theme::ColorfulTheme;
use std::env;

const SERVER_NAME: &str = "cgn";

pub fn install(_theme: &ColorfulTheme) {
    if let Err(e) = install_scripted() {
        eprintln!("Gemini CLI MCP install failed: {e}");
    }
}

pub fn uninstall(_theme: &ColorfulTheme) {
    if let Err(e) = uninstall_scripted() {
        eprintln!("Gemini CLI MCP uninstall failed: {e}");
    }
}

pub(crate) fn install_scripted() -> Result<(), CgnError> {
    let exe = env::current_exe()
        .map_err(|e| CgnError::Output(format!("current_exe: {e}")))?
        .to_string_lossy()
        .into_owned();

    println!("Registering cgn MCP server in Gemini CLI...");
    let output = gemini_cli::run(&["mcp", "add", SERVER_NAME, &exe, "admin", "mcp", "serve"])?;

    if output.status.success() {
        println!("✓ Gemini CLI MCP server 'cgn' added successfully.");
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        Err(CgnError::Output(format!(
            "gemini mcp add failed: {}",
            err.trim()
        )))
    }
}

pub(crate) fn uninstall_scripted() -> Result<(), CgnError> {
    let output = gemini_cli::run(&["mcp", "remove", SERVER_NAME])?;
    if output.status.success() {
        println!("✓ Gemini CLI MCP server 'cgn' removed.");
    } else {
        println!("Gemini CLI MCP server 'cgn' was not found or already removed.");
    }
    Ok(())
}

pub fn status() -> HostStatus {
    let Ok(output) = gemini_cli::run(&["mcp", "list"]) else {
        return HostStatus::Missing;
    };
    if !output.status.success() {
        return HostStatus::Missing;
    }
    let list = String::from_utf8_lossy(&output.stdout);
    if list.contains(SERVER_NAME) {
        HostStatus::Installed {
            detail: "managed via gemini mcp".into(),
        }
    } else {
        HostStatus::Missing
    }
}
