//! Manually-constructed MCP tool for `ecp schema` sub-subcommands.
//!
//! The root `ecp schema` subcommand is `#[command(hide = true)]` in main.rs,
//! so `enumerate_tools` skips it. Without this hand-rolled tool, LLM clients
//! would have no path to invoke `ecp schema <subcmd>` via MCP — the
//! `Schema` clap variant produces a nested-subcommand surface that the
//! generic clap-introspection layer cannot flatten into a single tool.
//!
//! Mirrors the `group.rs` / `peers.rs` pattern: one tool fronts every
//! sub-subcommand via a `subcmd` discriminator (`blindspots` / `reltypes`
//! / `node-kinds` / `graph-version`). `spawn::peel_subcmd` lifts `subcmd`
//! off the JSON object and prepends it as the first argv token, yielding
//! `ecp schema <subcmd> [--format <json|text>]`.
//!
//! The four read-only inventory views surface the LLM-utility justification
//! for every NodeKind / RelType variant and the per-language BlindSpot
//! emitter coverage so agents can decide which graph signals to trust
//! before composing a Cypher query.

use crate::schema::DerivedTool;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;

/// Return the single `ecp_schema` MCP tool fronting all schema-inventory
/// sub-subcommands.
pub fn schema_tools() -> Vec<DerivedTool> {
    vec![tool_schema()]
}

fn tool_schema() -> DerivedTool {
    DerivedTool {
        name: "ecp_schema".into(),
        subcommand: "schema".into(),
        description: "Read-only schema inventory: per-language BlindSpot \
            emitter coverage (`blindspots`), every RelType edge label with \
            LLM-utility tier (`reltypes`), every NodeKind variant with the \
            distinction that keeps it from being folded into a sibling \
            (`node-kinds`), and the rkyv graph.bin format version + bump \
            history (`graph-version`). Pick `subcmd`."
            .into(),
        schema: Arc::new(json!({
            "type": "object",
            "properties": {
                "subcmd": {
                    "type": "string",
                    "enum": ["blindspots", "reltypes", "node-kinds", "graph-version"],
                    "description": "Which schema inventory to emit. All four are graph-free (no .ecp/ load required)."
                },
                "format": {
                    "type": "string",
                    "enum": ["json", "text"],
                    "description": "[all] Output format. `json` (default) is structured; `text` is a human-readable table."
                }
            },
            "required": ["subcmd"],
            "additionalProperties": false
        })),
        flag_args: HashSet::new(),
        positional_args: Vec::new(),
        prefix_args: Vec::new(),
        subcmd_arg: Some("subcmd".into()),
    }
}
