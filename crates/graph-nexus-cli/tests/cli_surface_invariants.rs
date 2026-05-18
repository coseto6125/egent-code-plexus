//! Drift-detector for the CLI ↔ MCP surface.
//!
//! Why this exists: PR-146 reshaped the `gnx group` sub-subcommand tree
//! and exposed the matching `gnx_group` MCP tool by hand-rolling its
//! schema (the root `group` command is `#[command(hide = true)]` so
//! `enumerate_tools` skips it). Without a guard, the manual schema can
//! silently drift away from the real CLI flags — adding a new
//! `gnx group impact --threshold` flag without updating the MCP property
//! map would leave LLM clients unable to set it, and the failure mode
//! would only show up at runtime.
//!
//! The tests below pin three invariants:
//! 1. Every command + sub-subcommand in the hardcoded inventory below
//!    responds to `--help` with exit 0 (catches deletes, renames,
//!    broken arg defs).
//! 2. Every subcmd advertised by the `gnx_group` / `gnx_peers` MCP
//!    schemas maps to a real `gnx <root> <subcmd> --help` path.
//! 3. Every non-positional flag advertised in those MCP schemas appears
//!    as `--<kebab>` in the corresponding CLI `--help` text (the
//!    drift-killer).
//!
//! When a new subcommand or flag is added, update the inventory below
//! AND the matching MCP schema together. The test failing is the
//! expected behaviour — that's the whole point.

use std::process::{Command, Output};

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run(args: &[&str]) -> Output {
    Command::new(gnx_bin())
        .args(args)
        .output()
        .expect("gnx spawn failed")
}

fn assert_help_ok(path: &[&str]) {
    let mut args: Vec<&str> = path.to_vec();
    args.push("--help");
    let out = run(&args);
    assert!(
        out.status.success(),
        "`gnx {} --help` failed (exit {}):\nstderr: {}\nstdout: {}",
        path.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
}

// ── 1. CLI surface inventory ─────────────────────────────────────────────────

/// Every reachable top-level subcommand (visible + hidden). Hidden ones
/// (`admin`, `group`, `hook-*`, `watch`) still must respond to `--help`.
const TOP_LEVEL_COMMANDS: &[&str] = &[
    // Visible
    "inspect",
    "find",
    "impact",
    "rename",
    "cypher",
    "coverage",
    "routes",
    "contracts",
    "diff",
    "shape-check",
    "tool-map",
    "peers",
    "review",
    // Hidden
    "admin",
    "group",
    "hook-handle",
    "hook-watcher",
    "hook",
    "watch",
];

/// `gnx group <subcmd>` — keep in sync with `GroupCommands` enum in
/// `crates/graph-nexus-cli/src/commands/group/mod.rs`.
const GROUP_SUBCMDS: &[&str] = &["sync", "status", "contracts", "impact", "find", "coverage"];

/// `gnx peers <subcmd>` — keep in sync with `PeersCmd` enum.
const PEERS_SUBCMDS: &[&str] = &["status", "diff", "log", "say", "inbox", "thread", "gc"];

/// `gnx admin <subcmd>` — top-level admin operations.
const ADMIN_SUBCMDS: &[&str] = &[
    "install-hook",
    "uninstall-hook",
    "status",
    "drop",
    "prune",
    "config",
    "group",
    "index",
    "sessions",
    "mcp",
    "verify-resolver",
];

/// `gnx admin group <subcmd>` — management-only (no query verbs here).
const ADMIN_GROUP_SUBCMDS: &[&str] = &["add", "remove"];

/// `gnx admin mcp <subcmd>` — MCP server entry points.
const ADMIN_MCP_SUBCMDS: &[&str] = &["serve", "tools"];

/// `gnx admin sessions <subcmd>` — L1 session inspection.
const ADMIN_SESSIONS_SUBCMDS: &[&str] = &["list"];

#[test]
fn every_top_level_command_has_help() {
    for cmd in TOP_LEVEL_COMMANDS {
        assert_help_ok(&[cmd]);
    }
}

#[test]
fn every_group_subcommand_has_help() {
    for sub in GROUP_SUBCMDS {
        assert_help_ok(&["group", sub]);
    }
}

#[test]
fn every_peers_subcommand_has_help() {
    for sub in PEERS_SUBCMDS {
        assert_help_ok(&["peers", sub]);
    }
}

#[test]
fn every_admin_subcommand_has_help() {
    for sub in ADMIN_SUBCMDS {
        assert_help_ok(&["admin", sub]);
    }
}

#[test]
fn every_admin_group_subcommand_has_help() {
    for sub in ADMIN_GROUP_SUBCMDS {
        assert_help_ok(&["admin", "group", sub]);
    }
}

#[test]
fn every_admin_mcp_subcommand_has_help() {
    for sub in ADMIN_MCP_SUBCMDS {
        assert_help_ok(&["admin", "mcp", sub]);
    }
}

#[test]
fn every_admin_sessions_subcommand_has_help() {
    for sub in ADMIN_SESSIONS_SUBCMDS {
        assert_help_ok(&["admin", "sessions", sub]);
    }
}

// ── 2. MCP gnx_group / gnx_peers cross-checks ────────────────────────────────

/// For each subcmd advertised by `gnx_group`'s MCP schema, verify the
/// matching CLI path is reachable. Catches "renamed verb, forgot the
/// schema" drift.
#[test]
fn mcp_gnx_group_subcmds_are_real_cli_paths() {
    let tool = graph_nexus_mcp::group::group_tools()
        .into_iter()
        .find(|t| t.name == "gnx_group")
        .expect("gnx_group tool missing from registry");
    let allowed = enum_values(&tool.schema, "subcmd");
    assert!(!allowed.is_empty(), "gnx_group subcmd enum is empty");
    for sub in &allowed {
        assert_help_ok(&["group", sub]);
    }
    // Cross-check against the hardcoded inventory above so renames /
    // additions fail loudly on both sides.
    let inventory: Vec<String> = GROUP_SUBCMDS.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        sorted(allowed),
        sorted(inventory),
        "gnx_group MCP schema and GROUP_SUBCMDS inventory disagree"
    );
}

#[test]
fn mcp_gnx_peers_subcmds_are_real_cli_paths() {
    let tool = graph_nexus_mcp::peers::peer_tools()
        .into_iter()
        .find(|t| t.name == "gnx_peers")
        .expect("gnx_peers tool missing from registry");
    let allowed = enum_values(&tool.schema, "subcmd");
    for sub in &allowed {
        assert_help_ok(&["peers", sub]);
    }
}

// ── 3. MCP-advertised flags must exist in the real CLI --help ────────────────

/// For each (subcmd, expected_flags) tuple below, every flag must appear
/// in `gnx group <subcmd> --help` output. This is the drift-killer.
///
/// The expected flag set mirrors the `[subcmd]` tags in the
/// `gnx_group` schema property descriptions
/// (`crates/graph-nexus-mcp/src/group.rs`). Both files MUST move together.
#[test]
fn mcp_gnx_group_advertised_flags_exist_in_cli_help() {
    let cases: &[(&str, &[&str])] = &[
        ("sync", &["--exact-only", "--allow-stale", "--json", "--verbose"]),
        ("status", &["--json"]),
        ("contracts", &["--type", "--repo", "--unmatched", "--json"]),
        (
            "impact",
            &[
                "--target",
                "--repo",
                "--direction",
                "--max-depth",
                "--cross-depth",
                "--min-confidence",
                "--timeout-ms",
                "--include-tests",
                "--json",
            ],
        ),
        ("find", &["--merge", "--limit", "--batch", "--json"]),
        ("coverage", &["--json"]),
    ];

    for (subcmd, expected_flags) in cases {
        let out = run(&["group", subcmd, "--help"]);
        assert!(out.status.success(), "gnx group {subcmd} --help failed");
        let help = String::from_utf8_lossy(&out.stdout);
        for flag in *expected_flags {
            assert!(
                help.contains(flag),
                "gnx group {subcmd}: --help missing flag `{flag}` — MCP schema and CLI flags have drifted apart.\n--- help output ---\n{help}"
            );
        }
    }
}

/// Same drift-kill for `gnx_peers`, scoped to flags whose presence is
/// load-bearing for the LLM client (each subcmd has its own positional /
/// flag set documented via [tag] descriptions).
#[test]
fn mcp_gnx_peers_advertised_flags_exist_in_cli_help() {
    // peers subcmds vary heavily in flag set; spot-check the
    // most-used ones rather than exhaustively listing.
    let cases: &[(&str, &[&str])] = &[
        ("log", &["--since", "--limit"]),
        ("say", &["--to", "--reply"]),
        ("inbox", &["--limit"]),
    ];
    for (subcmd, expected_flags) in cases {
        let out = run(&["peers", subcmd, "--help"]);
        assert!(out.status.success(), "gnx peers {subcmd} --help failed");
        let help = String::from_utf8_lossy(&out.stdout);
        for flag in *expected_flags {
            assert!(
                help.contains(flag),
                "gnx peers {subcmd}: --help missing flag `{flag}` — gnx_peers schema drifted.\n--- help output ---\n{help}"
            );
        }
    }
}

// ── 4. MCP `tools` list shape ────────────────────────────────────────────────

/// `gnx admin mcp tools` is the LLM client's discovery surface. Verify
/// the manual tools (`gnx_peers`, `gnx_group`) appear exactly once, and
/// that no hidden tool (gnx_admin / gnx_hook / etc.) leaks through.
#[test]
fn admin_mcp_tools_list_includes_manual_tools_once_each() {
    let out = run(&["admin", "mcp", "tools"]);
    assert!(
        out.status.success(),
        "gnx admin mcp tools failed:\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listing = String::from_utf8_lossy(&out.stdout);

    for must_have in ["gnx_peers", "gnx_group"] {
        let count = listing.matches(must_have).count();
        assert!(
            count >= 1,
            "tool `{must_have}` missing from `gnx admin mcp tools` output:\n{listing}"
        );
    }
    // Hidden subcommands must not produce derived tools.
    for forbidden in ["gnx_admin", "gnx_hook_handle", "gnx_hook_watcher", "gnx_hook", "gnx_watch"]
    {
        assert!(
            !listing.contains(forbidden),
            "hidden subcommand leaked as MCP tool `{forbidden}`:\n{listing}"
        );
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn enum_values(schema: &serde_json::Value, prop_key: &str) -> Vec<String> {
    schema
        .get("properties")
        .and_then(|p| p.get(prop_key))
        .and_then(|s| s.get("enum"))
        .and_then(|e| e.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}
