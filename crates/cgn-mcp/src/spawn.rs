//! Spawn-mode dispatch: each tool call → `Command::new(cgn).arg(sub).args(argv).output()`.

use crate::schema::DerivedTool;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::Path;

/// Invoke `<binary> <subcommand> [subcmd_arg?] [prefix_args...] [argv...]` and
/// return captured stdout. If `tool.subcmd_arg` is set, the matching JSON key
/// is peeled out and prepended as the first arg (sub-subcommand router).
/// Non-zero exit → `Err` carrying stderr.
pub fn run_spawn(binary: &Path, tool: &DerivedTool, args: &Value) -> Result<String> {
    let (peeled_subcmd, json_args) = peel_subcmd(tool, args)?;
    let json_argv = crate::argv::json_to_argv(&json_args, &tool.flag_args, &tool.positional_args)?;
    let argv: Vec<&str> = peeled_subcmd
        .as_deref()
        .into_iter()
        .chain(tool.prefix_args.iter().map(String::as_str))
        .chain(json_argv.iter().map(String::as_str))
        .collect();
    let output = std::process::Command::new(binary)
        .arg(&tool.subcommand)
        .args(&argv)
        .output()
        .with_context(|| format!("spawning {binary:?} {}", tool.subcommand))?;
    if !output.status.success() {
        return Err(anyhow!(
            "cgn {} exited with {} — stderr:\n{}",
            tool.subcommand,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// If `tool.subcmd_arg = Some(key)`, lift the matching JSON string out and
/// return it alongside an `args` object with that key removed. Validates
/// the value against the schema's `subcmd.enum` if present.
fn peel_subcmd(tool: &DerivedTool, args: &Value) -> Result<(Option<String>, Value)> {
    let Some(key) = tool.subcmd_arg.as_deref() else {
        return Ok((None, args.clone()));
    };
    let map = args.as_object().ok_or_else(|| {
        anyhow!("expected JSON object at args root for subcmd-bearing tool {key}")
    })?;
    let val = map
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required `{key}` discriminator"))?
        .to_string();
    if let Some(allowed) = schema_enum(&tool.schema, key) {
        if !allowed.iter().any(|s| s == &val) {
            return Err(anyhow!(
                "`{key}` must be one of {allowed:?}, got {val:?}"
            ));
        }
    }
    let mut filtered = map.clone();
    filtered.remove(key);
    Ok((Some(val), Value::Object(filtered)))
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
