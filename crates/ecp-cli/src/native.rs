//! Native host adapter surface for Codex-style integrations.
//!
//! The adapter intentionally reuses the MCP command discovery path: clap
//! remains the source of truth for tool schemas, and JSON arguments are
//! converted through the same argv converter MCP spawn-mode uses. This gives
//! native hosts one stable typed envelope without hand-writing every command's
//! schema up front.
//!
//! TODO(native-tools): Codex registry wiring is still pending, so
//! `ecp admin codex install native-tools` does not expose this adapter yet.

use crate::cli::Cli;
use clap::CommandFactory;
use ecp_core::EcpError;
use ecp_mcp::schema::{ecp_tools, DerivedTool};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Query,
    Mutation,
    LongRunning,
    Hook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultFormat {
    Json,
    Text,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeTool {
    pub name: String,
    pub subcommand: String,
    pub description: String,
    pub schema: Value,
    pub kind: ToolKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub command: String,
    pub kind: ToolKind,
    pub stdout: String,
    pub stderr: String,
    pub format: ToolResultFormat,
    pub structured: Option<Value>,
}

pub fn tools() -> Vec<NativeTool> {
    ecp_tools(&Cli::command())
        .into_iter()
        .map(|tool| NativeTool {
            kind: classify_tool(&tool),
            schema: (*tool.schema).clone(),
            name: tool.name,
            subcommand: tool.subcommand,
            description: tool.description,
        })
        .collect()
}

pub fn tool_argv(name: &str, args: Value) -> Result<Vec<String>, EcpError> {
    let tool = find_tool(name)?;
    argv_for_tool(&tool, args)
}

pub fn call_spawn(binary: &Path, name: &str, args: Value) -> Result<ToolResult, EcpError> {
    let tool = find_tool(name)?;
    let kind = classify_tool(&tool);
    let (args, format) = prefer_json_args(&tool, args);
    let stdout = ecp_mcp::spawn::run_spawn(binary, &tool, &args)
        .map_err(|e| EcpError::Output(e.to_string()))?;
    let structured = match format {
        ToolResultFormat::Json => serde_json::from_str(stdout.trim()).ok(),
        ToolResultFormat::Text => None,
    };
    Ok(ToolResult {
        command: tool.subcommand,
        kind,
        stdout,
        stderr: String::new(),
        format,
        structured,
    })
}

fn find_tool(name: &str) -> Result<DerivedTool, EcpError> {
    ecp_tools(&Cli::command())
        .into_iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| EcpError::InvalidArgument(format!("unknown ecp native tool: {name}")))
}

fn argv_for_tool(tool: &DerivedTool, args: Value) -> Result<Vec<String>, EcpError> {
    let (peeled_subcmd, args) = peel_subcmd(tool, args)?;
    let json_argv = ecp_mcp::argv::json_to_argv(&args, &tool.flag_args, &tool.positional_args)
        .map_err(|e| EcpError::InvalidArgument(e.to_string()))?;
    let mut argv = Vec::with_capacity(1 + tool.prefix_args.len() + json_argv.len());
    argv.push(tool.subcommand.clone());
    if let Some(subcmd) = peeled_subcmd {
        argv.push(subcmd);
    }
    argv.extend(tool.prefix_args.iter().cloned());
    argv.extend(json_argv);
    Ok(argv)
}

fn peel_subcmd(tool: &DerivedTool, args: Value) -> Result<(Option<String>, Value), EcpError> {
    let Some(key) = tool.subcmd_arg.as_deref() else {
        return Ok((None, args));
    };
    let Value::Object(mut map) = args else {
        return Err(EcpError::InvalidArgument(format!(
            "expected JSON object at args root for subcmd-bearing tool {key}"
        )));
    };
    let val = map
        .remove(key)
        .and_then(|v| v.as_str().map(str::to_string))
        .ok_or_else(|| {
            EcpError::InvalidArgument(format!("missing required `{key}` discriminator"))
        })?;
    if let Some(allowed) = schema_enum(&tool.schema, key) {
        if !allowed.iter().any(|candidate| candidate == &val) {
            return Err(EcpError::InvalidArgument(format!(
                "`{key}` must be one of {allowed:?}, got {val:?}"
            )));
        }
    }
    Ok((Some(val), Value::Object(map)))
}

fn schema_enum(schema: &Value, prop_key: &str) -> Option<Vec<String>> {
    schema
        .get("properties")?
        .get(prop_key)?
        .get("enum")?
        .as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn prefer_json_args(tool: &DerivedTool, args: Value) -> (Value, ToolResultFormat) {
    let has_format = tool
        .schema
        .get("properties")
        .and_then(|props| props.get("format"))
        .is_some();
    if !has_format {
        return (args, ToolResultFormat::Text);
    }
    let Value::Object(mut map) = args else {
        return (args, ToolResultFormat::Text);
    };
    map.entry("format")
        .or_insert_with(|| Value::String("json".into()));
    (Value::Object(map), ToolResultFormat::Json)
}

fn classify_tool(tool: &DerivedTool) -> ToolKind {
    match tool.subcommand.as_str() {
        "rename" => ToolKind::Mutation,
        "watch" | "mcp" => ToolKind::LongRunning,
        "hook" | "hook-handle" | "hook-watcher" => ToolKind::Hook,
        _ => ToolKind::Query,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tools_include_visible_and_hand_rolled_surfaces() {
        let names: Vec<String> = tools().into_iter().map(|tool| tool.name).collect();
        assert!(names.contains(&"ecp_find".to_string()));
        assert!(names.contains(&"ecp_peers".to_string()));
        assert!(names.contains(&"ecp_group".to_string()));
        assert!(names.contains(&"ecp_schema".to_string()));
    }

    #[test]
    fn tool_argv_converts_json_args() {
        let argv = tool_argv(
            "ecp_find",
            json!({"pattern": "Engine", "repo": ".", "mode": "exact"}),
        )
        .expect("argv");
        assert_eq!(argv[0], "find");
        assert!(argv.contains(&"Engine".to_string()));
        assert!(argv.contains(&"--repo".to_string()));
        assert!(argv.contains(&".".to_string()));
    }

    #[test]
    fn subcmd_discriminator_is_peeled() {
        let argv = tool_argv("ecp_schema", json!({"subcmd": "node-kinds"})).expect("argv");
        assert_eq!(argv, vec!["schema", "node-kinds"]);
    }
}
