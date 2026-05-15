//! Convert the MCP-side JSON args object into the argv vector
//! `gnx <subcommand>` expects.
//!
//! `to_kebab` lowercases + replaces `_` / camelCase with `-`, matching the
//! `Rust ident → --kebab-flag` convention clap derive uses. The agent can
//! send either form (`includeTests` or `include_tests`); both produce
//! `--include-tests`.
//!
//! Positional args precede flags (clap accepts either order in practice;
//! we prepend for legibility in spawn logs). Boolean flag-style args
//! (`--verbose`) are emitted as bare flags when true and dropped when
//! false; boolean value-style args (`--high-trust-only true`) are emitted
//! with their value. The distinction is supplied by `schema::DerivedTool`.

use anyhow::{bail, Result};
use serde_json::Value;
use std::collections::HashSet;

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

/// Convert a JSON args object into argv. `flag_args` lists arg IDs that
/// are bare boolean flags (no following value); `positional_args` lists
/// arg IDs that are positional, in declared order.
pub fn json_to_argv(
    args: &Value,
    flag_args: &HashSet<String>,
    positional_args: &[String],
) -> Result<Vec<String>> {
    let Value::Object(map) = args else {
        bail!("expected JSON object at args root, got {}", type_name(args));
    };
    let positional_set: HashSet<&str> = positional_args.iter().map(|s| s.as_str()).collect();
    let mut out = Vec::with_capacity(map.len() * 2);

    // Positional first, in declared order.
    for pos_id in positional_args {
        let Some(v) = map.get(pos_id) else { continue };
        if let Some(s) = value_as_string(v) {
            out.push(s);
        }
    }

    for (k, v) in map {
        if positional_set.contains(k.as_str()) {
            continue;
        }
        let flag = format!("--{}", to_kebab(k));
        match v {
            Value::Null => continue,
            Value::Bool(b) => {
                if flag_args.contains(k) {
                    if *b {
                        out.push(flag);
                    }
                } else {
                    out.push(flag);
                    out.push(b.to_string());
                }
            }
            Value::String(s) => {
                out.push(flag);
                out.push(s.clone());
            }
            Value::Number(n) => {
                out.push(flag);
                out.push(n.to_string());
            }
            Value::Array(_) | Value::Object(_) => {
                bail!("nested array/object args not supported (key={k})");
            }
        }
    }
    Ok(out)
}

fn value_as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn flag_set(s: &[&str]) -> HashSet<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn positional_first_then_flags() {
        let args = json!({"pattern": "auth", "repo": "myrepo"});
        let argv = json_to_argv(&args, &flag_set(&[]), &["pattern".to_string()]).unwrap();
        assert_eq!(argv[0], "auth");
        assert!(argv.contains(&"--repo".to_string()));
    }

    #[test]
    fn flag_bool_emits_bare_when_true() {
        let args = json!({"verbose": true});
        let argv = json_to_argv(&args, &flag_set(&["verbose"]), &[]).unwrap();
        assert_eq!(argv, vec!["--verbose"]);
    }

    #[test]
    fn flag_bool_dropped_when_false() {
        let args = json!({"verbose": false});
        let argv = json_to_argv(&args, &flag_set(&["verbose"]), &[]).unwrap();
        assert!(argv.is_empty());
    }

    #[test]
    fn value_bool_emits_with_value() {
        // `high_trust_only` is action=Set with bool parser → value-style.
        let args = json!({"high_trust_only": false});
        let argv = json_to_argv(&args, &flag_set(&[]), &[]).unwrap();
        assert_eq!(argv, vec!["--high-trust-only", "false"]);
    }

    #[test]
    fn camel_case_key_becomes_kebab() {
        let args = json!({"includeTests": true});
        let argv = json_to_argv(&args, &flag_set(&["includeTests"]), &[]).unwrap();
        assert_eq!(argv, vec!["--include-tests"]);
    }
}
