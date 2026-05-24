//! Gemini CLI MCP integration — called from `ecp admin gemini` via
//! `install_scripted` / `uninstall_scripted` / `status`. No interactive
//! `install(_theme)` wrapper: Gemini CLI is host-first elevated, so all
//! entry points go through the scriptable surface.

use crate::admin::host_integration::gemini_cli;
use crate::admin::status::HostStatus;
use ecp_core::EcpError;
use std::env;

const SERVER_NAME: &str = "ecp";

pub(crate) fn install_scripted() -> Result<(), EcpError> {
    let exe = env::current_exe()
        .map_err(|e| EcpError::Output(format!("current_exe: {e}")))?
        .to_string_lossy()
        .into_owned();

    println!("Registering ecp MCP server in Gemini CLI...");
    let output = gemini_cli::run(&["mcp", "add", SERVER_NAME, &exe, "admin", "mcp", "serve"])?;

    if output.status.success() {
        println!("✓ Gemini CLI MCP server 'ecp' added successfully.");
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        Err(EcpError::Output(format!(
            "gemini mcp add failed: {}",
            err.trim()
        )))
    }
}

pub(crate) fn uninstall_scripted() -> Result<(), EcpError> {
    let output = gemini_cli::run(&["mcp", "remove", SERVER_NAME])?;
    if output.status.success() {
        println!("✓ Gemini CLI MCP server 'ecp' removed.");
    } else {
        println!("Gemini CLI MCP server 'ecp' was not found or already removed.");
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let pattern = format!("{}:", SERVER_NAME);
    if stdout.contains(&pattern) || stderr.contains(&pattern) {
        HostStatus::Installed {
            detail: "managed via gemini mcp".into(),
        }
    } else {
        HostStatus::Missing
    }
}
