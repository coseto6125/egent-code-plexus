//! Spawn-mode dispatch: each tool call → `Command::new(gnx).arg(sub).args(argv).output()`.

use crate::schema::DerivedTool;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::Path;

/// Invoke `<binary> <subcommand> [argv...]` and return captured stdout.
/// Non-zero exit → `Err` carrying stderr.
pub fn run_spawn(binary: &Path, tool: &DerivedTool, args: &Value) -> Result<String> {
    let argv = crate::argv::json_to_argv(args, &tool.flag_args, &tool.positional_args)?;
    let output = std::process::Command::new(binary)
        .arg(&tool.subcommand)
        .args(&argv)
        .output()
        .with_context(|| format!("spawning {binary:?} {}", tool.subcommand))?;
    if !output.status.success() {
        return Err(anyhow!(
            "gnx {} exited with {} — stderr:\n{}",
            tool.subcommand,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
