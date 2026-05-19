//! Manually-constructed MCP tool for `cgn peers` sub-subcommands.
//!
//! `enumerate_tools` would surface `cgn peers` as a single opaque tool with
//! no usable args (the sub-subcommand sits one clap level below the visible
//! root). We replace that with a hand-rolled `cgn_peers` tool that carries
//! a `subcmd` discriminator (`status` / `diff` / `log` / `say` / `inbox` /
//! `thread`); `cgn peers gc` is intentionally omitted — it's a maintenance
//! op, not an agent action.
//!
//! Dispatch path: `spawn::peel_subcmd` lifts the JSON `subcmd` field out and
//! prepends it as the first arg, yielding `cgn peers <subcmd> [flags...]`.
//!
//! The whole feature is only useful when ≥2 LLM sessions are running with
//! peer-sync; the tool description leads with that constraint so single-
//! agent sessions don't reach for it.

use crate::schema::DerivedTool;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;

/// Return the single `cgn_peers` MCP tool fronting all peer sub-subcommands.
pub fn peer_tools() -> Vec<DerivedTool> {
    vec![tool_peers()]
}

fn tool_peers() -> DerivedTool {
    DerivedTool {
        name: "cgn_peers".into(),
        subcommand: "peers".into(),
        description: "[multi-agent peer-sync only — single-session has no peers] \
            Inspect / talk to other cgn watch sessions on this repo. \
            Pick `subcmd`; see each arg's [tag] for which subcmd uses it."
            .into(),
        schema: Arc::new(json!({
            "type": "object",
            "properties": {
                "subcmd": {
                    "type": "string",
                    "enum": ["status", "diff", "log", "say", "inbox", "thread"],
                    "description": "Which peer operation to run. Each subcmd uses a disjoint subset of the args below."
                },
                "peer": {
                    "type": "string",
                    "description": "[diff] Peer session ID (from subcmd=status). [log] Filter messages to/from this peer."
                },
                "symbol": {
                    "type": "string",
                    "description": "[diff] If set, only show dirty entries touching this symbol name."
                },
                "since": {
                    "type": "string",
                    "description": "[log] RFC3339 timestamp — only show messages after this."
                },
                "direction": {
                    "type": "string",
                    "description": "[log] Filter: 'in' or 'out'."
                },
                "limit": {
                    "type": "integer",
                    "description": "[log / inbox] Maximum entries to return (default 50)."
                },
                "body": {
                    "type": "string",
                    "description": "[say] Message body."
                },
                "to": {
                    "type": "string",
                    "description": "[say] Target peer session ID. Omit to broadcast."
                },
                "reply": {
                    "type": "string",
                    "description": "[say] msg_id this message is replying to."
                },
                "msg_id": {
                    "type": "string",
                    "description": "[thread] msg_id returned by say or seen in log."
                },
                "repo": {
                    "type": "string",
                    "description": "Path to the repo root (optional; defaults to cwd)."
                }
            },
            "required": ["subcmd"],
            "additionalProperties": false
        })),
        flag_args: HashSet::new(),
        // Union of positional sets across subcmds; each subcmd uses a disjoint
        // subset, so the LLM's JSON object will only carry the relevant keys.
        // Order matters within a subcmd (diff: peer→symbol); across subcmds
        // it's irrelevant since no two share a positional name.
        positional_args: vec![
            "peer".into(),
            "symbol".into(),
            "body".into(),
            "msg_id".into(),
        ],
        prefix_args: Vec::new(),
        subcmd_arg: Some("subcmd".into()),
    }
}
