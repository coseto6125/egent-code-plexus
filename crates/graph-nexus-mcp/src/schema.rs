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
use std::any::TypeId;
use std::collections::HashSet;
use std::sync::Arc;

// `TypeId::of` is `const fn` since Rust 1.85; we lean on that to keep
// the integer / float type sets in static arrays addressable by
// `.contains()` rather than a long `||` chain.
const INT_TIDS: [TypeId; 6] = [
    TypeId::of::<i64>(),
    TypeId::of::<i32>(),
    TypeId::of::<u64>(),
    TypeId::of::<u32>(),
    TypeId::of::<usize>(),
    TypeId::of::<isize>(),
];
const FLOAT_TIDS: [TypeId; 2] = [TypeId::of::<f64>(), TypeId::of::<f32>()];

#[derive(Debug, Clone)]
pub struct DerivedTool {
    /// MCP tool name (`gnx_<subcommand>`).
    pub name: String,
    /// CLI subcommand name (no prefix).
    pub subcommand: String,
    /// Human-readable description from clap `about`.
    pub description: String,
    /// JSON-schema object: `{type, properties, required}`. `Arc`'d so the
    /// dispatch path (`call_tool` → `spawn_blocking`) can hold a cheap
    /// owned handle without deep-cloning a nested `Value` tree on every
    /// MCP request.
    pub schema: Arc<Value>,
    /// Arg IDs whose CLI action is `SetTrue`/`SetFalse` — emitted as a bare
    /// flag (`--foo`) with no following value.
    pub flag_args: HashSet<String>,
    /// Arg IDs that are positional, in declared order.
    pub positional_args: Vec<String>,
    /// Fixed argv tokens prepended before JSON-derived args. Used for
    /// sub-subcommand dispatch (e.g. `["status"]` → `gnx peers status`).
    pub prefix_args: Vec<String>,
    /// If `Some(key)`, the JSON arg with this key is peeled out, validated,
    /// and prepended as the first prefix arg at dispatch time. Lets one
    /// MCP tool front many sub-subcommands via a `subcmd` discriminator.
    pub subcmd_arg: Option<String>,
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

    let schema = Arc::new(json!({
        "type": "object",
        "properties": Value::Object(properties),
        "required": Value::Array(required),
        "additionalProperties": false,
    }));

    DerivedTool {
        name,
        subcommand,
        description,
        schema,
        flag_args,
        positional_args,
        prefix_args: Vec::new(),
        subcmd_arg: None,
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
    // `tid` is a `clap::builder::AnyValueId`, not a `std::any::TypeId` —
    // it carries an internal `TypeId` and impls `PartialEq<TypeId>`, so the
    // direct comparisons below resolve to that impl. We iterate
    // `INT_TIDS` / `FLOAT_TIDS` rather than using `.contains(&tid)`
    // because `<[TypeId]>::contains` would require an `&TypeId` argument.
    let tid = arg.get_value_parser().type_id();
    if tid == TypeId::of::<bool>() {
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

    let type_str = if INT_TIDS.iter().any(|t| tid == *t) {
        "integer"
    } else if FLOAT_TIDS.iter().any(|t| tid == *t) {
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
