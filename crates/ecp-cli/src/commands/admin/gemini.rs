//! Scriptable Gemini host integration commands for AI agents.

use crate::admin::host_integration::mcp::gemini as mcp_gemini;
use crate::admin::host_integration::native::gemini as native_gemini;
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum GeminiCommands {
    /// Install a Gemini integration component.
    Install {
        #[command(subcommand)]
        component: GeminiComponent,
    },
    /// Remove a Gemini integration component.
    Uninstall {
        #[command(subcommand)]
        component: GeminiComponent,
    },
    /// Show all Gemini integration statuses.
    Status,
}

#[derive(Subcommand, Debug)]
pub enum GeminiComponent {
    /// Gemini native skill (linked from docs/skills/ecp).
    NativeSkill,
    /// Gemini MCP server (stdio registration).
    McpServer,
}

pub fn run(command: GeminiCommands) -> Result<(), ecp_core::EcpError> {
    match command {
        GeminiCommands::Install { component } => install(component),
        GeminiCommands::Uninstall { component } => uninstall(component),
        GeminiCommands::Status => print_status(),
    }
}

pub(crate) fn install(component: GeminiComponent) -> Result<(), ecp_core::EcpError> {
    match component {
        GeminiComponent::NativeSkill => native_gemini::install_scripted(),
        GeminiComponent::McpServer => mcp_gemini::install_scripted(),
    }
}

pub(crate) fn uninstall(component: GeminiComponent) -> Result<(), ecp_core::EcpError> {
    match component {
        GeminiComponent::NativeSkill => native_gemini::uninstall_scripted(),
        GeminiComponent::McpServer => mcp_gemini::uninstall_scripted(),
    }
}

pub(crate) fn print_status() -> Result<(), ecp_core::EcpError> {
    native_gemini::status().print("Gemini CLI native-skill");
    mcp_gemini::status().print("Gemini CLI mcp-server");
    Ok(())
}
