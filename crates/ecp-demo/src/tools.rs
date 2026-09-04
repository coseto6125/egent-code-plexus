//! The subset of the MCP tool surface a public demo may expose, and the
//! per-tool schema the browser renders a form from.

use clap::Command;
use ecp_mcp::schema::{ecp_tools, DerivedTool};
use serde::Serialize;
use serde_json::Value;

/// Read-only subcommands. Left out on purpose: `rename` edits files;
/// `uninstall` and `admin` mutate the host; `peers`, `group`, `usage`,
/// `review` and `diff` read session, group, telemetry or working-tree state
/// the fixed corpora never carry.
pub const ALLOWED: &[&str] = &[
    "find",
    "inspect",
    "impact",
    "routes",
    "contracts",
    "path",
    "cypher",
    "summary",
    "processes",
    "tool-map",
    "shape-check",
    "heuristics",
    "pattern",
    "schema",
];

/// Args the server owns. `repo` is set from the selected corpus, `graph`
/// would let a caller point `ecp` at any file in the container, `batch`
/// reads stdin the demo never provides.
pub const RESERVED_ARGS: &[&str] = &["repo", "graph", "batch"];

pub struct DemoTool {
    pub inner: DerivedTool,
    /// Whether `--repo` exists on this subcommand; the runner injects it only then.
    pub takes_repo: bool,
    /// `inner.schema` with the reserved args removed from `properties` and `required`.
    pub public_schema: Value,
}

#[derive(Serialize)]
pub struct ToolListing<'a> {
    pub name: &'a str,
    pub subcommand: &'a str,
    pub description: &'a str,
    pub schema: &'a Value,
    pub positional_args: &'a [String],
}

impl DemoTool {
    fn new(inner: DerivedTool) -> Self {
        let mut public_schema = (*inner.schema).clone();
        let takes_repo = public_schema
            .get("properties")
            .and_then(|p| p.get("repo"))
            .is_some();
        if let Some(props) = public_schema
            .get_mut("properties")
            .and_then(Value::as_object_mut)
        {
            for key in RESERVED_ARGS {
                props.remove(*key);
            }
        }
        if let Some(required) = public_schema
            .get_mut("required")
            .and_then(Value::as_array_mut)
        {
            required.retain(|v| !RESERVED_ARGS.iter().any(|r| v.as_str() == Some(r)));
        }
        Self {
            inner,
            takes_repo,
            public_schema,
        }
    }

    pub fn listing(&self) -> ToolListing<'_> {
        ToolListing {
            name: &self.inner.name,
            subcommand: &self.inner.subcommand,
            description: &self.inner.description,
            schema: &self.public_schema,
            positional_args: &self.inner.positional_args,
        }
    }

    /// The first reserved arg a caller tried to set, if any.
    pub fn reserved_arg_used<'a>(&self, args: &'a Value) -> Option<&'a str> {
        args.as_object()?
            .keys()
            .map(String::as_str)
            .find(|k| RESERVED_ARGS.contains(k))
    }
}

/// The allowlisted tools, in `ecp --help` order.
pub fn demo_tools(root: &Command) -> Vec<DemoTool> {
    ecp_tools(root)
        .into_iter()
        .filter(|t| ALLOWED.contains(&t.subcommand.as_str()))
        .map(DemoTool::new)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use ecp_cli::cli::Cli;
    use serde_json::json;

    fn tools() -> Vec<DemoTool> {
        demo_tools(&Cli::command())
    }

    #[test]
    fn demo_tools_exclude_every_mutating_or_stateful_subcommand() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().map(|t| t.inner.subcommand.as_str()).collect();
        for banned in [
            "rename",
            "uninstall",
            "peers",
            "group",
            "usage",
            "review",
            "diff",
        ] {
            assert!(!names.contains(&banned), "{banned} must not be exposed");
        }
        assert_eq!(
            names.len(),
            ALLOWED.len(),
            "every allowlisted subcommand resolves: {names:?}"
        );
    }

    #[test]
    fn public_schema_drops_reserved_args_but_keeps_the_rest() {
        let find = tools()
            .into_iter()
            .find(|t| t.inner.subcommand == "find")
            .expect("find is allowlisted");
        assert!(find.takes_repo);
        let props = find.public_schema["properties"].as_object().unwrap();
        for reserved in RESERVED_ARGS {
            assert!(
                !props.contains_key(*reserved),
                "{reserved} leaked into the public schema"
            );
        }
        assert!(
            props.contains_key("pattern"),
            "positional `pattern` survives"
        );
        assert!(props.contains_key("mode"), "`--mode` survives");
    }

    #[test]
    fn reserved_arg_used_names_the_offending_key() {
        let find = tools()
            .into_iter()
            .find(|t| t.inner.subcommand == "find")
            .unwrap();
        assert_eq!(
            find.reserved_arg_used(&json!({"pattern": "x", "graph": "/etc/passwd"})),
            Some("graph")
        );
        assert_eq!(find.reserved_arg_used(&json!({"pattern": "x"})), None);
    }
}
