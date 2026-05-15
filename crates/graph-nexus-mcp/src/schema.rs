//! Derive MCP tool metadata from gnx's `clap::Command` tree.
//!
//! Every visible (non-`hide`d) subcommand of the root command becomes one
//! tool. Each tool carries:
//! - `name` / `subcommand` — `gnx_<sub>` / `<sub>`
//! - `description` — the subcommand's `#[command(about = ...)]`
//! - `schema` — JSON-schema object reverse-engineered from the subcommand's
//!   `clap::Arg` set; `description` comes from `#[arg(help = ...)]`, and
//!   `type` / `enum` are inferred from the arg's `ValueParser` + `ArgAction`
//! - `flag_args` / `positional_args` — used by `argv` to translate the
//!   MCP-side JSON object back into the precise CLI shape clap expects
//!   (flags with no value vs. flags taking a value, positional ordering)

use clap::{Arg, ArgAction, Command};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct DerivedTool {
    /// MCP tool name (`gnx_<subcommand>`).
    pub name: String,
    /// CLI subcommand name (no prefix).
    pub subcommand: String,
    /// Human-readable description from clap `about`.
    pub description: String,
    /// JSON-schema object: `{type, properties, required}`.
    pub schema: Value,
    /// Arg IDs whose CLI action is `SetTrue`/`SetFalse` — emitted as a bare
    /// flag (`--foo`) with no following value.
    pub flag_args: HashSet<String>,
    /// Arg IDs that are positional, in declared order.
    pub positional_args: Vec<String>,
}

/// Enumerate every visible subcommand of `root` as an MCP tool.
pub fn enumerate_tools(root: &Command) -> Vec<DerivedTool> {
    root.get_subcommands()
        .filter(|c| !c.is_hide_set())
        .filter(|c| c.get_name() != "help")
        .map(derive_tool)
        .collect()
}

fn derive_tool(cmd: &Command) -> DerivedTool {
    let subcommand = cmd.get_name().to_string();
    let name = format!("gnx_{subcommand}");
    let description = cmd
        .get_about()
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("gnx {subcommand}"));

    let mut flag_args = HashSet::new();
    let mut positional_args = Vec::new();
    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();

    for arg in cmd.get_arguments() {
        if arg.is_hide_set() {
            continue;
        }
        let id = arg.get_id().to_string();
        if arg.is_positional() {
            positional_args.push(id.clone());
        }
        if matches!(
            arg.get_action(),
            ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Help | ArgAction::Version
        ) {
            flag_args.insert(id.clone());
        }
        properties.insert(id.clone(), Value::Object(build_property(arg)));
        if arg.is_required_set() {
            required.push(Value::String(id));
        }
    }

    let schema = json!({
        "type": "object",
        "properties": Value::Object(properties),
        "required": Value::Array(required),
        "additionalProperties": false,
    });

    DerivedTool {
        name,
        subcommand,
        description,
        schema,
        flag_args,
        positional_args,
    }
}

fn build_property(arg: &Arg) -> Map<String, Value> {
    let mut prop = Map::new();
    if let Some(help) = arg.get_help() {
        prop.insert("description".into(), Value::String(help.to_string()));
    }
    let (type_str, enum_values) = infer_type_and_enum(arg);
    prop.insert("type".into(), Value::String(type_str.into()));
    if let Some(values) = enum_values {
        prop.insert(
            "enum".into(),
            Value::Array(values.into_iter().map(Value::String).collect()),
        );
    }
    prop
}

fn infer_type_and_enum(arg: &Arg) -> (&'static str, Option<Vec<String>>) {
    // Booleans take priority over `possible_values()`: clap's BoolValueParser
    // advertises ["true", "false"] as possible values, but we want to expose
    // it as JSON `boolean`, not an enum-of-strings.
    let action = arg.get_action();
    if matches!(action, ArgAction::SetTrue | ArgAction::SetFalse) {
        return ("boolean", None);
    }
    if matches!(action, ArgAction::Count) {
        return ("integer", None);
    }
    let tid = arg.get_value_parser().type_id();
    if tid == std::any::TypeId::of::<bool>() {
        return ("boolean", None);
    }

    let pv: Vec<String> = arg
        .get_value_parser()
        .possible_values()
        .map(|iter| iter.map(|v| v.get_name().to_string()).collect())
        .unwrap_or_default();
    if !pv.is_empty() {
        return ("string", Some(pv));
    }

    let type_str = if tid == std::any::TypeId::of::<i64>()
        || tid == std::any::TypeId::of::<i32>()
        || tid == std::any::TypeId::of::<u64>()
        || tid == std::any::TypeId::of::<u32>()
        || tid == std::any::TypeId::of::<usize>()
        || tid == std::any::TypeId::of::<isize>()
    {
        "integer"
    } else if tid == std::any::TypeId::of::<f64>() || tid == std::any::TypeId::of::<f32>() {
        "number"
    } else {
        "string"
    };
    (type_str, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Args, CommandFactory, Parser, Subcommand};

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: Cmds,
    }

    #[derive(Subcommand)]
    enum Cmds {
        /// Visible tool.
        Foo(FooArgs),
        /// Hidden subcommand (should be skipped).
        #[command(hide = true)]
        Hidden,
    }

    #[derive(Args)]
    struct FooArgs {
        /// Pattern positional.
        pattern: String,
        /// Bool flag.
        #[arg(long)]
        verbose: bool,
        /// String option.
        #[arg(long)]
        repo: Option<String>,
        /// Integer option.
        #[arg(long, default_value_t = 5usize)]
        depth: usize,
    }

    #[test]
    fn enumerate_includes_only_visible() {
        let tools = enumerate_tools(&Cli::command());
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["gnx_foo"]);
    }

    #[test]
    fn derives_schema_types() {
        let tools = enumerate_tools(&Cli::command());
        let foo = &tools[0];
        let props = foo.schema["properties"].as_object().unwrap();
        assert_eq!(props["pattern"]["type"], "string");
        assert_eq!(props["verbose"]["type"], "boolean");
        assert_eq!(props["repo"]["type"], "string");
        assert_eq!(props["depth"]["type"], "integer");
    }

    #[test]
    fn tracks_positionals_and_flags() {
        let tools = enumerate_tools(&Cli::command());
        let foo = &tools[0];
        assert_eq!(foo.positional_args, vec!["pattern".to_string()]);
        assert!(foo.flag_args.contains("verbose"));
        assert!(!foo.flag_args.contains("repo"));
    }
}
