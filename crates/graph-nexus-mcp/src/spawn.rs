//! Spawn-mode dispatch. Each tool call → `Command::new(gnx).arg(subcmd).args(argv).output()`.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::Path;

/// Synchronously invoke `<binary> <subcommand> [argv...]` and return
/// captured stdout on success. Non-zero exit → Err containing stderr.
pub fn run_spawn(binary: &Path, subcommand: &str, args: &Value) -> Result<String> {
    let argv = crate::argv::json_to_argv(args)?;
    let output = std::process::Command::new(binary)
        .arg(subcommand)
        .args(&argv)
        .output()
        .with_context(|| format!("spawning {binary:?} {subcommand}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "gnx {subcommand} exited with {} — stderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
