//! Codex CLI MCP integration (no-fork side-car path).

use crate::admin::status::HostStatus;
use dialoguer::theme::ColorfulTheme;
use ecp_core::EcpError;
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value;

const SERVER_NAME: &str = "ecp";
const SERVER_ARGS: &[&str] = &["admin", "mcp", "serve"];

pub fn install(_theme: &ColorfulTheme) {
    match run_install() {
        Ok(path) => println!("Codex CLI MCP server `ecp` installed in {}", path.display()),
        Err(e) => eprintln!("Codex CLI MCP install failed: {e}"),
    }
}

pub fn uninstall(_theme: &ColorfulTheme) {
    match run_uninstall() {
        Ok(path) => println!("Codex CLI MCP server `ecp` removed from {}", path.display()),
        Err(e) => eprintln!("Codex CLI MCP uninstall failed: {e}"),
    }
}

pub fn status() -> HostStatus {
    let path = config_path();
    match read_config(&path) {
        Ok(config) => status_from_config(&config, &current_command()),
        Err(e) if !path.exists() => {
            let _ = e;
            HostStatus::Missing
        }
        Err(e) => HostStatus::Outdated {
            reason: format!("cannot read {}: {e}", path.display()),
        },
    }
}

pub(crate) fn run_install() -> Result<PathBuf, EcpError> {
    let path = config_path();
    upsert_server(&path, &current_command())?;
    Ok(path)
}

fn run_uninstall() -> Result<PathBuf, EcpError> {
    let path = config_path();
    remove_server(&path)?;
    Ok(path)
}

fn config_path() -> PathBuf {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(home).join("config.toml");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".codex").join("config.toml")
}

fn current_command() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "ecp".into())
}

fn upsert_server(path: &Path, command: &str) -> Result<(), EcpError> {
    let mut config = read_config(path)?;
    let root = config
        .as_table_mut()
        .ok_or_else(|| EcpError::InvalidArgument("Codex config root is not a TOML table".into()))?;
    let servers = root
        .entry("mcp_servers")
        .or_insert_with(|| Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| {
            EcpError::InvalidArgument("Codex config `mcp_servers` is not a TOML table".into())
        })?;

    let mut server = toml::map::Map::new();
    server.insert("command".into(), Value::String(command.into()));
    server.insert(
        "args".into(),
        Value::Array(
            SERVER_ARGS
                .iter()
                .map(|arg| Value::String((*arg).into()))
                .collect(),
        ),
    );
    server.insert("enabled".into(), Value::Boolean(true));
    servers.insert(SERVER_NAME.into(), Value::Table(server));

    write_config(path, &config)
}

fn remove_server(path: &Path) -> Result<(), EcpError> {
    if !path.exists() {
        return Ok(());
    }
    let mut config = read_config(path)?;
    if let Some(servers) = config
        .as_table_mut()
        .and_then(|root| root.get_mut("mcp_servers"))
        .and_then(Value::as_table_mut)
    {
        servers.remove(SERVER_NAME);
    }
    write_config(path, &config)
}

fn read_config(path: &Path) -> Result<Value, EcpError> {
    if !path.exists() {
        return Ok(Value::Table(toml::map::Map::new()));
    }
    let raw = fs::read_to_string(path)
        .map_err(|e| EcpError::Output(format!("read {}: {e}", path.display())))?;
    if raw.trim().is_empty() {
        return Ok(Value::Table(toml::map::Map::new()));
    }
    toml::from_str::<Value>(&raw)
        .map_err(|e| EcpError::InvalidArgument(format!("parse {}: {e}", path.display())))
}

fn write_config(path: &Path, config: &Value) -> Result<(), EcpError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = toml::to_string_pretty(config)
        .map_err(|e| EcpError::Serialization(format!("TOML encode: {e}")))?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, body)?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn status_from_config(config: &Value, command: &str) -> HostStatus {
    let Some(server) = config
        .get("mcp_servers")
        .and_then(|servers| servers.get(SERVER_NAME))
        .and_then(Value::as_table)
    else {
        return HostStatus::Missing;
    };
    let configured_command = server.get("command").and_then(Value::as_str);
    let configured_args = server.get("args").and_then(Value::as_array);
    let enabled = server
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let args_match = configured_args.is_some_and(|args| {
        args.len() == SERVER_ARGS.len()
            && args
                .iter()
                .zip(SERVER_ARGS.iter())
                .all(|(actual, expected)| actual.as_str() == Some(*expected))
    });
    if !enabled {
        return HostStatus::Outdated {
            reason: "mcp_servers.ecp is disabled".into(),
        };
    }
    if configured_command == Some(command) && args_match {
        HostStatus::Installed {
            detail: "mode=spawn".into(),
        }
    } else {
        HostStatus::Outdated {
            reason: "mcp_servers.ecp differs from current ecp admin mcp serve entry".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_server_preserves_other_codex_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
model = "gpt-5.1-codex-max"

[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
"#,
        )
        .expect("write seed");

        upsert_server(&path, "/usr/local/bin/ecp").expect("install");
        let config = read_config(&path).expect("read result");

        assert_eq!(
            config.get("model").and_then(Value::as_str),
            Some("gpt-5.1-codex-max")
        );
        assert!(config
            .get("mcp_servers")
            .and_then(|servers| servers.get("github"))
            .is_some());
        assert!(matches!(
            status_from_config(&config, "/usr/local/bin/ecp"),
            HostStatus::Installed { .. }
        ));
    }

    #[test]
    fn remove_server_keeps_other_servers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        upsert_server(&path, "/usr/local/bin/ecp").expect("install");
        remove_server(&path).expect("remove");
        let config = read_config(&path).expect("read result");

        assert!(matches!(
            status_from_config(&config, "/usr/local/bin/ecp"),
            HostStatus::Missing
        ));
    }

    #[test]
    fn remove_server_does_not_create_missing_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");

        remove_server(&path).expect("remove missing");

        assert!(!path.exists());
    }

    #[test]
    fn disabled_server_is_not_installed() {
        let config = toml::from_str::<Value>(
            r#"
[mcp_servers.ecp]
command = "/usr/local/bin/ecp"
args = ["admin", "mcp", "serve"]
enabled = false
"#,
        )
        .expect("parse config");

        assert!(matches!(
            status_from_config(&config, "/usr/local/bin/ecp"),
            HostStatus::Outdated { .. }
        ));
    }
}
