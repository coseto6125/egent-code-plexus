//! Claude Code MCP integration.

use crate::admin::status::HostStatus;
use dialoguer::theme::ColorfulTheme;
use graph_nexus_core::GnxError;
use serde_json::json;
use std::ffi::OsString;
use std::io;
use std::process::Command;

const SERVER_NAME: &str = "gnx";

pub fn install(_theme: &ColorfulTheme) {
    match run_install() {
        Ok(()) => println!("Claude Code MCP server `gnx` installed via `claude mcp`."),
        Err(e) => eprintln!("Claude Code MCP install failed: {e}"),
    }
}

pub fn uninstall(_theme: &ColorfulTheme) {
    match run_uninstall() {
        Ok(()) => println!("Claude Code MCP server `gnx` removed via `claude mcp`."),
        Err(e) => eprintln!("Claude Code MCP uninstall failed: {e}"),
    }
}

pub fn status() -> HostStatus {
    match claude_mcp(["mcp", "get", SERVER_NAME]) {
        Ok(output) => status_from_get_result(output.status.success()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => HostStatus::Missing,
        Err(e) => HostStatus::Outdated {
            reason: format!("claude mcp get failed: {e}"),
        },
    }
}

fn run_install() -> Result<(), GnxError> {
    let exe = std::env::current_exe()
        .map_err(|e| GnxError::Output(format!("current_exe: {e}")))?
        .to_string_lossy()
        .into_owned();
    let args = install_args(&exe);

    let output = Command::new("claude")
        .args(args)
        .output()
        .map_err(|e| GnxError::Output(format!("spawn claude: {e}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error("claude mcp add-json", &output))
    }
}

fn run_uninstall() -> Result<(), GnxError> {
    let output = claude_mcp(["mcp", "remove", SERVER_NAME])
        .map_err(|e| GnxError::Output(format!("spawn claude: {e}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error("claude mcp remove", &output))
    }
}

fn claude_mcp<const N: usize>(args: [&str; N]) -> std::io::Result<std::process::Output> {
    Command::new("claude").args(args).output()
}

fn install_args(exe: &str) -> Vec<OsString> {
    vec![
        "mcp".into(),
        "add-json".into(),
        "--scope".into(),
        "user".into(),
        SERVER_NAME.into(),
        server_spec(exe).into(),
    ]
}

fn server_spec(exe: &str) -> String {
    json!({
        "type": "stdio",
        "command": exe,
        "args": ["admin", "mcp", "serve"],
    })
    .to_string()
}

fn status_from_get_result(success: bool) -> HostStatus {
    if success {
        HostStatus::Installed {
            detail: "managed by claude mcp".into(),
        }
    } else {
        HostStatus::Missing
    }
}

fn command_error(command: &str, output: &std::process::Output) -> GnxError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    GnxError::Output(format!("{command}: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn install_args_register_user_scoped_stdio_server() {
        let args = install_args("/usr/local/bin/gnx");
        let text_args: Vec<_> = args.iter().map(|arg| arg.to_string_lossy()).collect();

        assert_eq!(text_args[0], "mcp");
        assert_eq!(text_args[1], "add-json");
        assert_eq!(text_args[2], "--scope");
        assert_eq!(text_args[3], "user");
        assert_eq!(text_args[4], SERVER_NAME);

        let spec: Value = serde_json::from_str(&text_args[5]).expect("json spec");
        assert_eq!(spec["type"], "stdio");
        assert_eq!(spec["command"], "/usr/local/bin/gnx");
        assert_eq!(spec["args"], json!(["admin", "mcp", "serve"]));
    }

    #[test]
    fn status_from_get_result_maps_success_and_missing() {
        assert!(matches!(
            status_from_get_result(true),
            HostStatus::Installed { .. }
        ));
        assert!(matches!(status_from_get_result(false), HostStatus::Missing));
    }
}
