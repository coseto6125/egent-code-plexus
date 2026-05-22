//! Drift-detector for the CLI ↔ MCP surface.
//!
//! Why this exists: PR-146 reshaped the `ecp group` sub-subcommand tree
//! and exposed the matching `ecp_group` MCP tool by hand-rolling its
//! schema (the root `group` command is `#[command(hide = true)]` so
//! `enumerate_tools` skips it). Without a guard, the manual schema can
//! silently drift away from the real CLI flags — adding a new
//! `ecp group impact --threshold` flag without updating the MCP property
//! map would leave LLM clients unable to set it, and the failure mode
//! would only show up at runtime.
//!
//! The tests below pin three invariants:
//! 1. Every command + sub-subcommand in the hardcoded inventory below
//!    responds to `--help` with exit 0 (catches deletes, renames,
//!    broken arg defs).
//! 2. Every subcmd advertised by the `ecp_group` / `ecp_peers` MCP
//!    schemas maps to a real `ecp <root> <subcmd> --help` path.
//! 3. Every non-positional flag advertised in those MCP schemas appears
//!    as `--<kebab>` in the corresponding CLI `--help` text (the
//!    drift-killer).
//!
//! When a new subcommand or flag is added, update the inventory below
//! AND the matching MCP schema together. The test failing is the
//! expected behaviour — that's the whole point.

use std::process::{Command, Output};

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn run(args: &[&str]) -> Output {
    Command::new(ecp_bin())
        .args(args)
        .output()
        .expect("ecp spawn failed")
}

fn assert_help_ok(path: &[&str]) {
    let mut args: Vec<&str> = path.to_vec();
    args.push("--help");
    let out = run(&args);
    assert!(
        out.status.success(),
        "`ecp {} --help` failed (exit {}):\nstderr: {}\nstdout: {}",
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
    "find-transaction-patterns",
    "find-schema-bindings",
    "find-event-mirrors",
    // Hidden
    "admin",
    "group",
    "hook-handle",
    "hook-watcher",
    "hook",
    "watch",
];

/// `ecp group <subcmd>` — keep in sync with `GroupCommands` enum in
/// `crates/ecp-cli/src/commands/group/mod.rs`.
const GROUP_SUBCMDS: &[&str] = &["sync", "status", "contracts", "impact", "find", "coverage"];

/// `ecp peers <subcmd>` — keep in sync with `PeersCmd` enum.
const PEERS_SUBCMDS: &[&str] = &["status", "diff", "log", "say", "inbox", "thread", "gc"];

/// `ecp admin <subcmd>` — top-level admin operations.
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
    "claude",
    "codex",
    "gemini",
    "mcp",
    "verify-resolver",
];

/// `ecp admin group <subcmd>` — management-only (no query verbs here).
const ADMIN_GROUP_SUBCMDS: &[&str] = &["add", "remove"];

/// `ecp admin mcp <subcmd>` — MCP server entry points.
const ADMIN_MCP_SUBCMDS: &[&str] = &["serve", "tools"];

/// `ecp admin codex <action>` — Codex host integration actions.
const ADMIN_CODEX_SUBCMDS: &[&str] = &["install", "uninstall", "status"];

/// `ecp admin codex install <component>` — installable Codex components.
const ADMIN_CODEX_INSTALL_SUBCMDS: &[&str] = &["native-tools", "skills"];

/// `ecp admin codex uninstall <component>` — removable Codex components.
const ADMIN_CODEX_UNINSTALL_SUBCMDS: &[&str] = &["native-tools", "skills"];

/// `ecp admin codex install skills <target>` — installable Codex skill targets.
const ADMIN_CODEX_SKILL_TARGETS: &[&str] = &["all", "ecp", "simplify"];

/// `ecp admin gemini <action>` — Gemini host integration actions.
const ADMIN_GEMINI_SUBCMDS: &[&str] = &["install", "uninstall", "status"];

/// `ecp admin gemini install <component>` — installable Gemini components.
const ADMIN_GEMINI_INSTALL_SUBCMDS: &[&str] = &["native-skill", "mcp-server"];

/// `ecp admin gemini uninstall <component>` — removable Gemini components.
const ADMIN_GEMINI_UNINSTALL_SUBCMDS: &[&str] = &["native-skill", "mcp-server"];

/// `ecp admin claude <action>` — Claude Code host integration actions.
const ADMIN_CLAUDE_SUBCMDS: &[&str] = &["install", "uninstall", "status"];

/// `ecp admin claude install <component>` — installable Claude Code components.
const ADMIN_CLAUDE_INSTALL_SUBCMDS: &[&str] = &["hooks", "mcp-server", "skills"];

/// `ecp admin claude uninstall <component>` — removable Claude Code components.
const ADMIN_CLAUDE_UNINSTALL_SUBCMDS: &[&str] = &["hooks", "mcp-server", "skills"];

/// `ecp admin claude install skills <target>` — installable Claude skill targets.
const ADMIN_CLAUDE_SKILL_TARGETS: &[&str] = &["all", "simplify"];

/// `ecp admin sessions <subcmd>` — L1 session inspection.
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
fn every_admin_codex_subcommand_has_help() {
    for sub in ADMIN_CODEX_SUBCMDS {
        assert_help_ok(&["admin", "codex", sub]);
    }
}

#[test]
fn every_admin_codex_install_subcommand_has_help() {
    for sub in ADMIN_CODEX_INSTALL_SUBCMDS {
        assert_help_ok(&["admin", "codex", "install", sub]);
    }
}

#[test]
fn every_admin_codex_uninstall_subcommand_has_help() {
    for sub in ADMIN_CODEX_UNINSTALL_SUBCMDS {
        assert_help_ok(&["admin", "codex", "uninstall", sub]);
    }
}

#[test]
fn every_admin_codex_install_skills_target_has_help() {
    for sub in ADMIN_CODEX_SKILL_TARGETS {
        assert_help_ok(&["admin", "codex", "install", "skills", sub]);
    }
}

#[test]
fn every_admin_codex_uninstall_skills_target_has_help() {
    for sub in ADMIN_CODEX_SKILL_TARGETS {
        assert_help_ok(&["admin", "codex", "uninstall", "skills", sub]);
    }
}

#[test]
fn every_admin_gemini_subcommand_has_help() {
    for sub in ADMIN_GEMINI_SUBCMDS {
        assert_help_ok(&["admin", "gemini", sub]);
    }
}

#[test]
fn every_admin_gemini_install_subcommand_has_help() {
    for sub in ADMIN_GEMINI_INSTALL_SUBCMDS {
        assert_help_ok(&["admin", "gemini", "install", sub]);
    }
}

#[test]
fn every_admin_gemini_uninstall_subcommand_has_help() {
    for sub in ADMIN_GEMINI_UNINSTALL_SUBCMDS {
        assert_help_ok(&["admin", "gemini", "uninstall", sub]);
    }
}

#[test]
fn every_admin_claude_subcommand_has_help() {
    for sub in ADMIN_CLAUDE_SUBCMDS {
        assert_help_ok(&["admin", "claude", sub]);
    }
}

#[test]
fn every_admin_claude_install_subcommand_has_help() {
    for sub in ADMIN_CLAUDE_INSTALL_SUBCMDS {
        assert_help_ok(&["admin", "claude", "install", sub]);
    }
}

#[test]
fn every_admin_claude_uninstall_subcommand_has_help() {
    for sub in ADMIN_CLAUDE_UNINSTALL_SUBCMDS {
        assert_help_ok(&["admin", "claude", "uninstall", sub]);
    }
}

#[test]
fn every_admin_claude_install_skills_target_has_help() {
    for sub in ADMIN_CLAUDE_SKILL_TARGETS {
        assert_help_ok(&["admin", "claude", "install", "skills", sub]);
    }
}

#[test]
fn every_admin_claude_uninstall_skills_target_has_help() {
    for sub in ADMIN_CLAUDE_SKILL_TARGETS {
        assert_help_ok(&["admin", "claude", "uninstall", "skills", sub]);
    }
}

#[test]
fn every_admin_sessions_subcommand_has_help() {
    for sub in ADMIN_SESSIONS_SUBCMDS {
        assert_help_ok(&["admin", "sessions", sub]);
    }
}

// ── 2. MCP ecp_group / ecp_peers cross-checks ────────────────────────────────

/// For each subcmd advertised by `ecp_group`'s MCP schema, verify the
/// matching CLI path is reachable. Catches "renamed verb, forgot the
/// schema" drift.
#[test]
fn mcp_ecp_group_subcmds_are_real_cli_paths() {
    let tool = ecp_mcp::group::group_tools()
        .into_iter()
        .find(|t| t.name == "ecp_group")
        .expect("ecp_group tool missing from registry");
    let allowed = enum_values(&tool.schema, "subcmd");
    assert!(!allowed.is_empty(), "ecp_group subcmd enum is empty");
    for sub in &allowed {
        assert_help_ok(&["group", sub]);
    }
    // Cross-check against the hardcoded inventory above so renames /
    // additions fail loudly on both sides.
    let inventory: Vec<String> = GROUP_SUBCMDS.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        sorted(allowed),
        sorted(inventory),
        "ecp_group MCP schema and GROUP_SUBCMDS inventory disagree"
    );
}

#[test]
fn mcp_ecp_peers_subcmds_are_real_cli_paths() {
    let tool = ecp_mcp::peers::peer_tools()
        .into_iter()
        .find(|t| t.name == "ecp_peers")
        .expect("ecp_peers tool missing from registry");
    let allowed = enum_values(&tool.schema, "subcmd");
    for sub in &allowed {
        assert_help_ok(&["peers", sub]);
    }
}

// ── 3. MCP-advertised flags must exist in the real CLI --help ────────────────

/// For each (subcmd, expected_flags) tuple below, every flag must appear
/// in `ecp group <subcmd> --help` output. This is the drift-killer.
///
/// The expected flag set mirrors the `[subcmd]` tags in the
/// `ecp_group` schema property descriptions
/// (`crates/ecp-mcp/src/group.rs`). Both files MUST move together.
#[test]
fn mcp_ecp_group_advertised_flags_exist_in_cli_help() {
    let cases: &[(&str, &[&str])] = &[
        (
            "sync",
            &["--exact-only", "--allow-stale", "--json", "--verbose"],
        ),
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
        assert!(out.status.success(), "ecp group {subcmd} --help failed");
        let help = String::from_utf8_lossy(&out.stdout);
        for flag in *expected_flags {
            assert!(
                help.contains(flag),
                "ecp group {subcmd}: --help missing flag `{flag}` — MCP schema and CLI flags have drifted apart.\n--- help output ---\n{help}"
            );
        }
    }
}

/// Same drift-kill for `ecp_peers`, scoped to flags whose presence is
/// load-bearing for the LLM client (each subcmd has its own positional /
/// flag set documented via [tag] descriptions).
#[test]
fn mcp_ecp_peers_advertised_flags_exist_in_cli_help() {
    // peers subcmds vary heavily in flag set; spot-check the
    // most-used ones rather than exhaustively listing.
    let cases: &[(&str, &[&str])] = &[
        ("log", &["--since", "--limit"]),
        ("say", &["--to", "--reply"]),
        ("inbox", &["--limit"]),
    ];
    for (subcmd, expected_flags) in cases {
        let out = run(&["peers", subcmd, "--help"]);
        assert!(out.status.success(), "ecp peers {subcmd} --help failed");
        let help = String::from_utf8_lossy(&out.stdout);
        for flag in *expected_flags {
            assert!(
                help.contains(flag),
                "ecp peers {subcmd}: --help missing flag `{flag}` — ecp_peers schema drifted.\n--- help output ---\n{help}"
            );
        }
    }
}

// ── 4. MCP `tools` list shape ────────────────────────────────────────────────

/// `ecp admin mcp tools` is the LLM client's discovery surface. Verify
/// the manual tools (`ecp_peers`, `ecp_group`) appear exactly once, and
/// that no hidden tool (ecp_admin / ecp_hook / etc.) leaks through.
#[test]
fn admin_mcp_tools_list_includes_manual_tools_once_each() {
    let out = run(&["admin", "mcp", "tools"]);
    assert!(
        out.status.success(),
        "ecp admin mcp tools failed:\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listing = String::from_utf8_lossy(&out.stdout);

    for must_have in ["ecp_peers", "ecp_group"] {
        let count = listing.matches(must_have).count();
        assert!(
            count >= 1,
            "tool `{must_have}` missing from `ecp admin mcp tools` output:\n{listing}"
        );
    }
    // Hidden subcommands must not produce derived tools.
    for forbidden in [
        "ecp_admin",
        "ecp_hook_handle",
        "ecp_hook_watcher",
        "ecp_hook",
        "ecp_watch",
    ] {
        assert!(
            !listing.contains(forbidden),
            "hidden subcommand leaked as MCP tool `{forbidden}`:\n{listing}"
        );
    }
}

// ── 5. Reverse-direction flag check (CLI → MCP) ──────────────────────────────

/// For each `ecp group <subcmd>` and `ecp peers <subcmd>`, every `--flag`
/// shown in the real `--help` must exist as a property in the MCP schema
/// (after kebab→snake conversion). This is the asymmetric partner to
/// `mcp_ecp_group_advertised_flags_exist_in_cli_help` — without both
/// directions, a CLI-side flag addition silently leaves LLM clients
/// unable to set it.
///
/// Skipped:
/// - `--help` / `-h` (always present, never in schema)
/// - `--graph` (global flag declared on the root Cli; not subcmd-specific)
#[test]
fn cli_group_flags_all_exist_in_mcp_group_schema() {
    let tool = ecp_mcp::group::group_tools()
        .into_iter()
        .find(|t| t.name == "ecp_group")
        .expect("ecp_group tool");
    let props = tool
        .schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("properties object");

    let subcmds = enum_values(&tool.schema, "subcmd");
    for subcmd in &subcmds {
        let out = run(&["group", subcmd, "--help"]);
        let help = String::from_utf8_lossy(&out.stdout);
        for flag_kebab in extract_long_flags(&help) {
            let prop_key = flag_kebab.replace('-', "_");
            assert!(
                props.contains_key(&prop_key),
                "ecp group {subcmd}: CLI flag `--{flag_kebab}` (schema key `{prop_key}`) is missing from ecp_group MCP schema. Add a property entry — otherwise LLM clients cannot reach this flag.\n--- help ---\n{help}"
            );
        }
    }
}

#[test]
fn cli_peers_flags_all_exist_in_mcp_peers_schema() {
    let tool = ecp_mcp::peers::peer_tools()
        .into_iter()
        .find(|t| t.name == "ecp_peers")
        .expect("ecp_peers tool");
    let props = tool
        .schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("properties object");

    let subcmds = enum_values(&tool.schema, "subcmd");
    for subcmd in &subcmds {
        let out = run(&["peers", subcmd, "--help"]);
        let help = String::from_utf8_lossy(&out.stdout);
        for flag_kebab in extract_long_flags(&help) {
            let prop_key = flag_kebab.replace('-', "_");
            assert!(
                props.contains_key(&prop_key),
                "ecp peers {subcmd}: CLI flag `--{flag_kebab}` (schema key `{prop_key}`) missing from ecp_peers MCP schema.\n--- help ---\n{help}"
            );
        }
    }
}

// ── 6. Dynamic inventory diff vs hardcoded tables ────────────────────────────

/// `ecp --help` lists every **visible** top-level subcommand. The
/// hardcoded `TOP_LEVEL_COMMANDS` table must be a SUPERSET — every
/// visible command appears in the inventory, but the inventory may
/// carry extras (hidden subcommands like `admin` / `hook-*`).
///
/// Catches: "added a new visible top-level verb but forgot the
/// invariant table".
#[test]
fn top_level_inventory_covers_all_visible_commands() {
    let out = run(&["--help"]);
    let help = String::from_utf8_lossy(&out.stdout);
    let visible = extract_subcommands_from_help(&help);
    for cmd in &visible {
        assert!(
            TOP_LEVEL_COMMANDS.contains(&cmd.as_str()),
            "visible top-level command `{cmd}` is missing from TOP_LEVEL_COMMANDS inventory in this test file. Add it."
        );
    }
}

#[test]
fn group_inventory_covers_all_subcommands() {
    let out = run(&["group", "--help"]);
    let help = String::from_utf8_lossy(&out.stdout);
    let listed = extract_subcommands_from_help(&help);
    for cmd in &listed {
        assert!(
            GROUP_SUBCMDS.contains(&cmd.as_str()),
            "group subcommand `{cmd}` missing from GROUP_SUBCMDS inventory"
        );
    }
}

#[test]
fn peers_inventory_covers_all_subcommands() {
    let out = run(&["peers", "--help"]);
    let help = String::from_utf8_lossy(&out.stdout);
    let listed = extract_subcommands_from_help(&help);
    for cmd in &listed {
        assert!(
            PEERS_SUBCMDS.contains(&cmd.as_str()),
            "peers subcommand `{cmd}` missing from PEERS_SUBCMDS inventory"
        );
    }
}

#[test]
fn admin_inventory_covers_all_subcommands() {
    let out = run(&["admin", "--help"]);
    let help = String::from_utf8_lossy(&out.stdout);
    let listed = extract_subcommands_from_help(&help);
    for cmd in &listed {
        assert!(
            ADMIN_SUBCMDS.contains(&cmd.as_str()),
            "admin subcommand `{cmd}` missing from ADMIN_SUBCMDS inventory"
        );
    }
}

// ── 7. Schema semantic invariants ────────────────────────────────────────────

/// Every `[subcmd]` tag inside `ecp_group` schema property descriptions
/// must reference a subcmd that actually exists in the enum (or the
/// special tag `[all]` meaning "applies to all subcmds"). Catches:
/// rename a subcmd, forget to update tag references in descriptions.
#[test]
fn mcp_ecp_group_description_tags_reference_valid_subcmds() {
    let tool = ecp_mcp::group::group_tools()
        .into_iter()
        .find(|t| t.name == "ecp_group")
        .expect("ecp_group tool");

    let mut valid: Vec<String> = enum_values(&tool.schema, "subcmd");
    valid.push("all".to_string()); // meta-tag covering every subcmd

    let props = tool
        .schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("properties");

    for (prop_name, prop) in props {
        let Some(desc) = prop.get("description").and_then(|v| v.as_str()) else {
            continue;
        };
        for tag in extract_bracket_tags(desc) {
            assert!(
                valid.iter().any(|v| v == &tag),
                "property `{prop_name}` description references unknown [tag] `[{tag}]` — does not match any subcmd in enum {valid:?}\n  description: {desc}"
            );
        }
    }
}

/// Every name in `required` must be a defined property. Catches: rename
/// a property without updating the required list, or vice versa.
#[test]
fn mcp_ecp_group_required_keys_are_defined_properties() {
    let tool = ecp_mcp::group::group_tools()
        .into_iter()
        .find(|t| t.name == "ecp_group")
        .expect("ecp_group tool");
    let props = tool
        .schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("properties");
    let required = tool
        .schema
        .get("required")
        .and_then(|r| r.as_array())
        .expect("required array");
    for key in required {
        let key = key.as_str().expect("required entry must be string");
        assert!(
            props.contains_key(key),
            "ecp_group: required key `{key}` not in properties map"
        );
    }
}

/// Every positional_args entry must be a defined property too — the
/// dispatch layer (`argv::json_to_argv`) looks up by that name.
#[test]
fn mcp_ecp_group_positional_args_are_defined_properties() {
    let tool = ecp_mcp::group::group_tools()
        .into_iter()
        .find(|t| t.name == "ecp_group")
        .expect("ecp_group tool");
    let props = tool
        .schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("properties");
    for pos in &tool.positional_args {
        assert!(
            props.contains_key(pos),
            "ecp_group: positional arg `{pos}` not declared in properties — dispatch will silently drop it"
        );
    }
}

// ── 8. End-to-end MCP smoke (JSON-RPC over stdio) ────────────────────────────

/// Spawn `ecp admin mcp serve`, perform MCP initialize handshake, send
/// `tools/list`, verify ecp_group + ecp_peers + a derived tool all
/// appear. Catches: rmcp transport regression, schema-serialisation
/// bug, server-loop deadlock.
///
/// Marked `#[ignore]` — runs on demand via
/// `cargo test --test cli_surface_invariants -- --ignored` and in CI
/// once we wire it into the workflow. Reason: stdio JSON-RPC handshake
/// is timing-sensitive and the few-hundred-ms server startup adds CI
/// flake risk; keep the always-on suite fast.
#[test]
#[ignore = "spawns subprocess + stdio JSON-RPC; opt-in via --ignored"]
fn mcp_serve_responds_to_initialize_and_tools_list() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    let mut child = Command::new(ecp_bin())
        .args(["admin", "mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn mcp serve");

    let mut stdin = child.stdin.take().expect("stdin pipe");
    let stdout = child.stdout.take().expect("stdout pipe");
    let mut reader = BufReader::new(stdout);

    // 1. Initialize handshake
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2024-11-05","capabilities":{{}},"clientInfo":{{"name":"cli-surface-test","version":"0"}}}}}}"#
    )
    .expect("write initialize");

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("read initialize response");
    let init: serde_json::Value =
        serde_json::from_str(&line).expect("initialize response must be JSON");
    assert_eq!(init["id"], 1, "id mismatch on initialize: {line}");
    assert!(
        init["result"]["serverInfo"]["name"].is_string(),
        "initialize result missing serverInfo.name: {line}"
    );

    // 2. Notify initialized (no response expected)
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .expect("write initialized notification");

    // 3. tools/list
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list"}}"#)
        .expect("write tools/list");

    line.clear();
    reader
        .read_line(&mut line)
        .expect("read tools/list response");
    let list: serde_json::Value =
        serde_json::from_str(&line).expect("tools/list response must be JSON");
    assert_eq!(list["id"], 2, "id mismatch on tools/list: {line}");
    let tools = list["result"]["tools"]
        .as_array()
        .expect("tools array missing");
    let names: Vec<String> = tools
        .iter()
        .filter_map(|t| t["name"].as_str().map(String::from))
        .collect();
    for required_tool in ["ecp_group", "ecp_peers", "ecp_find", "ecp_impact"] {
        assert!(
            names.iter().any(|n| n == required_tool),
            "tool `{required_tool}` missing from tools/list response. Got: {names:?}"
        );
    }

    // 4. Clean shutdown.
    let _ = child.kill();
    let _ = child.wait();
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Pull `name` tokens out of a `--help` Commands: section. Lines look like
/// `  <name>     <one-line description>`. Stops at the blank line that
/// terminates the section. `help` is filtered out (clap auto-injects it).
fn extract_subcommands_from_help(help: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_commands = false;
    for line in help.lines() {
        let trimmed_left = line.trim_start();
        if line.starts_with("Commands:") {
            in_commands = true;
            continue;
        }
        if !in_commands {
            continue;
        }
        if trimmed_left.is_empty() {
            break; // section terminator
        }
        // Line shape: leading whitespace, then identifier, then spaces, then desc.
        let first_token = trimmed_left.split_whitespace().next().unwrap_or("");
        if first_token.is_empty() || first_token == "help" {
            continue;
        }
        // Defensive: token must be a clap-style command identifier.
        if first_token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            out.push(first_token.to_string());
        }
    }
    out
}

/// Pull `--long-flag` tokens out of a clap --help dump. Only lines whose
/// trim-leading content STARTS with `--` are treated as flag declarations
/// — that's how clap formats its Options: section. Description / docstring
/// lines that mention `--mode` mid-sentence are ignored on purpose.
///
/// Strips the leading `--`, skips `help` (always present, never in MCP)
/// and `graph` (global flag declared on the root Cli, not per-subcmd).
/// Dedupes within one help dump.
fn extract_long_flags(help: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::<String>::new();
    for line in help.lines() {
        let trimmed = line.trim_start();
        // Only lines that start strictly with `--<alpha>` qualify as flag
        // declarations. Short-form pairings like `-h, --help` are skipped
        // — the only flag that comes paired with a short form is `--help`
        // itself, which we filter out below anyway.
        let Some(rest) = trimmed.strip_prefix("--") else {
            continue;
        };
        if !rest.starts_with(|c: char| c.is_ascii_alphabetic()) {
            continue;
        }
        // Extract flag name up to the first non-ident char.
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if name.is_empty() || matches!(name.as_str(), "help" | "graph") {
            continue;
        }
        if seen.insert(name.clone()) {
            out.push(name);
        }
    }
    out
}

/// Extract `[tag]` markers from a description string. Tags are
/// lowercase identifiers (allowing `_` and `/` for compound tags).
fn extract_bracket_tags(desc: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = desc.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b']' {
                end += 1;
            }
            if end < bytes.len() {
                let tag = &desc[start..end];
                if !tag.is_empty()
                    && tag.chars().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '/'
                    })
                {
                    out.push(tag.to_string());
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

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
