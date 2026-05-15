//! Convert MCP-side JSON args into the clap CLI flag form gnx
//! subcommands expect.
//!
//! Used by spawn-mode dispatch. Daemon mode never goes through this —
//! it passes the JSON straight to each command's `run_inner` which
//! takes its already-typed `Args` struct via serde_json::from_value.

use anyhow::{bail, Result};
use serde_json::Value;

/// Map camelCase → kebab-case for clap long flag form. clap by default
/// converts the Rust field name `include_tests` to `--include-tests`,
/// so JSON callers using `includeTests` need this translation.
fn to_kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else if c == '_' {
            out.push('-');
        } else {
            out.push(c);
        }
    }
    out
}

pub fn json_to_argv(args: &Value) -> Result<Vec<String>> {
    let Value::Object(map) = args else {
        bail!("expected JSON object at args root, got {}", type_name(args));
    };
    let mut out = Vec::with_capacity(map.len() * 2);
    for (k, v) in map {
        let flag = format!("--{}", to_kebab(k));
        match v {
            Value::Null => continue,
            Value::Bool(true) => out.push(flag),
            Value::Bool(false) => continue,
            Value::String(s) => {
                out.push(flag);
                out.push(s.clone());
            }
            Value::Number(n) => {
                out.push(flag);
                out.push(n.to_string());
            }
            Value::Array(_) | Value::Object(_) => {
                bail!(
                    "nested array/object args not supported (key={k}); flatten or use daemon mode"
                );
            }
        }
    }
    Ok(out)
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
