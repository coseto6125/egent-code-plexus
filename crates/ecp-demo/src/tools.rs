//! The subset of the MCP tool surface a public demo may expose, and the
//! per-tool schema the browser renders a form from.

use clap::Command;
use ecp_mcp::schema::{ecp_tools, DerivedTool};
use serde::Serialize;
use serde_json::Value;

/// Read-only subcommands. Left out on purpose: `rename` edits files;
/// `uninstall` and `admin` mutate the host; `peers`, `group`, `usage`,
/// `review` and `diff` read session, group, telemetry or working-tree state
/// a fresh checkout never carries.
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

/// Flags the server owns. `repo` is set from the selected checkout, `graph`
/// would let a caller point `ecp` at any file in the container (it is a
/// clap global, so every subcommand accepts it), `batch` reads stdin the
/// demo never provides.
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
}

/// The first reserved flag among argv tokens, matched the way clap parses
/// them: `--graph`, `--graph=…`. Checked on the translated argv rather than
/// on JSON keys, so key spelling (`Graph` → `--graph`) and a positional
/// value that starts with `--` are both caught.
pub fn reserved_token(argv: &[String]) -> Option<&'static str> {
    argv.iter().find_map(|token| {
        let name = token.strip_prefix("--")?.split('=').next()?;
        RESERVED_ARGS.iter().copied().find(|r| *r == name)
    })
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
    use ecp_mcp::spawn::build_argv;
    use serde_json::json;

    fn tools() -> Vec<DemoTool> {
        demo_tools(&Cli::command())
    }

    fn find_tool() -> DemoTool {
        tools()
            .into_iter()
            .find(|t| t.inner.subcommand == "find")
            .expect("find is allowlisted")
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
        let find = find_tool();
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
    fn reserved_token_catches_key_spelling_and_positional_smuggling() {
        let find = find_tool();
        let argv = |args: Value| build_argv(&find.inner, &args).unwrap();
        assert_eq!(
            reserved_token(&argv(json!({"pattern": "x", "graph": "/etc/passwd"}))),
            Some("graph")
        );
        assert_eq!(
            reserved_token(&argv(json!({"pattern": "x", "Graph": "/etc/passwd"}))),
            Some("graph"),
            "`Graph` kebab-cases to --graph"
        );
        assert_eq!(
            reserved_token(&argv(json!({"pattern": "--graph=/etc/shadow"}))),
            Some("graph"),
            "a positional value is passed verbatim and clap reads it as the flag"
        );
        assert_eq!(
            reserved_token(&argv(json!({"pattern": "x", "repo": "/"}))),
            Some("repo")
        );
        assert_eq!(
            reserved_token(&argv(json!({"pattern": "x", "mode": "fuzzy", "all": true}))),
            None
        );
        assert_eq!(
            reserved_token(&argv(json!({"pattern": "graph"}))),
            None,
            "a bare word is not a flag"
        );
    }
}
