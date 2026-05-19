# Tier B CLI Surface + `cgn diff` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `mcp` / `verify_resolver` to `admin` namespace, un-hide `shape_check` at top-level with `--route`, and introduce `cgn diff --section <bindings|routes|contracts|all> --baseline <ref>` as the generalized graph-level cross-commit diff command.

**Architecture:** Visibility / namespace refactor for three existing hidden commands; new `commands/diff/` module containing baseline-ref resolution, git stash/checkout RAII helper, three section diff implementations, and three output formatters. Uses existing `cgn admin index --dump-resolver` for bindings dump; loads two `graph.bin` files for routes / contracts comparison.

**Tech Stack:** Rust 2021, clap 4.5 derive, serde_json, rkyv-archived graph, git CLI (via `std::process::Command`), `gh` CLI (for PR/<n> resolution), existing `cgn_core::graph` + `cgn_core::analyzer::types` APIs.

**Spec reference:** `docs/superpowers/specs/2026-05-16-tier-b-surface-and-diff-design.md`

---

## File Structure

### Files to modify

| File | Change |
|---|---|
| `crates/cgn-cli/src/main.rs` | Remove `Commands::Mcp` / `Commands::VerifyResolver` variants + their dispatch; remove `#[command(hide = true)]` on `Commands::ShapeCheck`; add new `Commands::Diff(commands::diff::DiffArgs)` variant + dispatch |
| `crates/cgn-cli/src/commands/mod.rs` | Add `pub mod diff;` |
| `crates/cgn-cli/src/commands/admin/mod.rs` | Add `Mcp(McpArgs)` + `VerifyResolver(VerifyResolverArgs)` variants to `AdminCommands` enum + dispatch in `run()` |
| `crates/cgn-cli/src/commands/shape_check.rs` | Add `route: Option<String>` to `ShapeCheckArgs`; filter Fetches edges by route path in `run()` |
| `crates/cgn-cli/src/commands/mcp.rs` | Add `format: Option<OutputFormat>` to `McpAction::Tools`; emit tools list via configured formatter |
| `~/.claude/skills/cgn/SKILL.md` | Append rows for `shape_check --route` and `diff --section --baseline`; note `mcp` / `verify-resolver` are admin-namespaced |

### Files to create

| File | Responsibility |
|---|---|
| `crates/cgn-cli/src/commands/diff/mod.rs` | `DiffArgs` struct, dispatch entry, section composition |
| `crates/cgn-cli/src/commands/diff/baseline.rs` | Resolve `--baseline <ref>` to commit SHA (branch / tag / SHA / HEAD~N / `PR/<n>`) |
| `crates/cgn-cli/src/commands/diff/git_guard.rs` | RAII helper that stashes dirty tree + checkouts SHA + restores on drop |
| `crates/cgn-cli/src/commands/diff/bindings.rs` | `bindings` section: compare two resolver-dump JSONLs |
| `crates/cgn-cli/src/commands/diff/routes.rs` | `routes` section: compare Route nodes between two graphs |
| `crates/cgn-cli/src/commands/diff/contracts.rs` | `contracts` section: compare cross-repo contracts |
| `crates/cgn-cli/src/commands/diff/output.rs` | Format text / json / toon |
| `crates/cgn-cli/tests/admin_mcp_test.rs` | Verify `admin mcp tools --format toon` works; not visible at top-level |
| `crates/cgn-cli/tests/admin_verify_resolver_test.rs` | Verify `admin verify-resolver` dispatch |
| `crates/cgn-cli/tests/shape_check_route_filter.rs` | Verify `--route` filter |
| `crates/cgn-cli/tests/diff_baseline_resolve_test.rs` | Verify baseline ref resolution variants |
| `crates/cgn-cli/tests/diff_bindings_test.rs` | Verify bindings section diff |
| `crates/cgn-cli/tests/diff_routes_test.rs` | Verify routes section diff |
| `crates/cgn-cli/tests/diff_contracts_test.rs` | Verify contracts section diff |
| `crates/cgn-cli/tests/diff_section_all_test.rs` | Verify `--section all` equivalence to multi-select |
| `crates/cgn-cli/tests/cli_help_surface_test.rs` | Snapshot top-level / admin `--help` |

---

## Task Decomposition

### Task 1: Move `Mcp` to `admin mcp`

**Files:**
- Modify: `crates/cgn-cli/src/commands/admin/mod.rs:14-39`
- Modify: `crates/cgn-cli/src/main.rs:80-83` (remove top-level variant), `:125-133` (remove early dispatch), `:148`, `:154-155`, `:191-192` (remove from no-graph fall-through arms)
- Test: `crates/cgn-cli/tests/admin_mcp_test.rs` (create)

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-cli/tests/admin_mcp_test.rs`:

```rust
use std::process::Command;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

#[test]
fn admin_mcp_tools_lists_tools() {
    let output = Command::new(cgn_bin())
        .args(["admin", "mcp", "tools"])
        .output()
        .expect("run cgn admin mcp tools");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("inspect"), "expected `inspect` tool in list, got: {stdout}");
}

#[test]
fn top_level_mcp_no_longer_visible() {
    let output = Command::new(cgn_bin())
        .args(["--help"])
        .output()
        .expect("run cgn --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("\n  mcp "),
        "mcp must NOT appear as top-level command in --help, got: {stdout}"
    );
}

#[test]
fn admin_mcp_appears_under_admin_help() {
    let output = Command::new(cgn_bin())
        .args(["admin", "--help"])
        .output()
        .expect("run cgn admin --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mcp"), "expected `mcp` subcommand under admin, got: {stdout}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p code-graph-nexus --test admin_mcp_test`
Expected: 2 tests fail (admin_mcp_tools_lists_tools: "unrecognized subcommand"; admin_mcp_appears_under_admin_help: missing). top_level_mcp_no_longer_visible may already pass if `Mcp` is `#[command(hide = true)]`.

- [ ] **Step 3: Add `Mcp` variant to `AdminCommands` enum**

Edit `crates/cgn-cli/src/commands/admin/mod.rs`:

```rust
pub mod claude_code;
pub mod config;
pub mod drop;
pub mod group;
pub mod index;
pub mod install_hook;
pub mod prune;
pub mod rename_branch;

#[derive(Subcommand, Debug)]
pub enum AdminCommands {
    /// Install git ref-transaction hook for branch tracking (or Claude Code hooks with --claude-code)
    InstallHook(install_hook::InstallHookArgs),
    /// Remove Claude Code hook entries from settings.json
    UninstallHook(claude_code::UninstallHookArgs),
    /// Show Claude Code hook install status
    Status(claude_code::StatusArgs),
    /// Delete a repo's index data + registry entry
    Drop(drop::DropArgs),
    /// Remove orphan index dirs not in registry
    Prune(prune::PruneArgs),
    /// Rename a branch's index dir
    RenameBranch(rename_branch::RenameBranchArgs),
    /// Interactive TOML config editor
    Config(config::ConfigArgs),
    /// Manage repo group membership
    Group {
        #[command(subcommand)]
        command: group::GroupCommands,
    },
    /// Build or refresh the graph (explicit / bulk / embeddings)
    Index(index::IndexArgs),
    /// Run MCP server (serve) or list exposed tools (tools).
    Mcp(crate::commands::mcp::McpArgs),
}

pub fn run(
    cmd: AdminCommands,
    root_cmd: clap::Command,
) -> Result<(), cgn_core::CgnError> {
    match cmd {
        AdminCommands::InstallHook(args) => install_hook::run(args),
        AdminCommands::UninstallHook(args) => claude_code::run_uninstall(args),
        AdminCommands::Status(args) => claude_code::run_status(args),
        AdminCommands::Drop(args) => drop::run(args),
        AdminCommands::Prune(args) => prune::run(args),
        AdminCommands::RenameBranch(args) => rename_branch::run(args),
        AdminCommands::Config(args) => config::run(args),
        AdminCommands::Group { command } => group::run(command),
        AdminCommands::Index(args) => index::run(args).map_err(cgn_core::CgnError::Output),
        AdminCommands::Mcp(args) => crate::commands::mcp::run(args, root_cmd),
    }
}
```

Note: `admin::run` signature now takes `root_cmd: clap::Command` because `commands::mcp::run` needs it for tool introspection.

- [ ] **Step 4: Remove top-level `Commands::Mcp` and update admin dispatch site**

Edit `crates/cgn-cli/src/main.rs`:

Remove these blocks:
- Top-level variant definition (search for `Mcp(commands::mcp::McpArgs)` around line 80-83 and delete the 3-line block)
- Top-level dispatch in the no-graph match (around line 132-134, the `Commands::Mcp(args) => { run_no_graph!(commands::mcp::run(args.clone(), Cli::command())) }` arm)
- The `Commands::Mcp(_)` arm in both later fall-through matches (around line 154-155 and 191-192)

Update the `Commands::Admin { command }` early dispatch (find around line 122-124, before the `match &cli.command`):

```rust
Commands::Admin { command } => {
    let root = Cli::command();
    return match command {
        Some(c) => commands::admin::run(c.clone(), root)
            .map(|()| std::process::ExitCode::SUCCESS)
            .unwrap_or_else(|e| {
                eprintln!("Command failed: {e}");
                std::process::ExitCode::from(1)
            }),
        None => match commands::admin::run_tui() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("Command failed: {e}");
                std::process::ExitCode::from(1)
            }
        },
    };
}
```

(If `commands::admin::run` was called without `root` previously, also add a `pub fn run_tui()` for the no-subcommand TUI path. Check actual code; the goal is single-entry for admin that passes `Cli::command()` when needed.)

- [ ] **Step 5: Run test to verify pass**

Run: `cargo build --workspace 2>&1 | tail -5`
Expected: build succeeds, no compile errors.

Run: `cargo test -p code-graph-nexus --test admin_mcp_test`
Expected: 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/commands/admin/mod.rs \
        crates/cgn-cli/src/main.rs \
        crates/cgn-cli/tests/admin_mcp_test.rs
git commit -m "refactor(admin): move mcp from top-level to admin namespace"
```

---

### Task 2: Move `VerifyResolver` to `admin verify-resolver`

**Files:**
- Modify: `crates/cgn-cli/src/commands/admin/mod.rs` (add variant + dispatch)
- Modify: `crates/cgn-cli/src/main.rs` (remove top-level)
- Test: `crates/cgn-cli/tests/admin_verify_resolver_test.rs` (create)

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-cli/tests/admin_verify_resolver_test.rs`:

```rust
use std::process::Command;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

#[test]
fn admin_verify_resolver_help_lists_required_args() {
    let output = Command::new(cgn_bin())
        .args(["admin", "verify-resolver", "--help"])
        .output()
        .expect("run cgn admin verify-resolver --help");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--oracle"), "expected --oracle arg in help: {stdout}");
    assert!(stdout.contains("--cgn"), "expected --cgn arg in help: {stdout}");
}

#[test]
fn top_level_verify_resolver_no_longer_dispatches() {
    let output = Command::new(cgn_bin())
        .args(["verify-resolver", "--help"])
        .output()
        .expect("run cgn verify-resolver --help");
    // Should fail because top-level command was removed
    assert!(
        !output.status.success() || !String::from_utf8_lossy(&output.stdout).contains("oracle"),
        "verify-resolver must not be a top-level command"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p code-graph-nexus --test admin_verify_resolver_test`
Expected: `admin_verify_resolver_help_lists_required_args` fails with "unrecognized subcommand 'verify-resolver'".

- [ ] **Step 3: Add variant + dispatch**

Edit `crates/cgn-cli/src/commands/admin/mod.rs` — add to `AdminCommands` enum (after `Mcp`):

```rust
    /// Diff resolver dump against language oracle (cgn-dev QA)
    VerifyResolver(crate::commands::verify_resolver::VerifyResolverArgs),
```

Add dispatch arm in `run()`:

```rust
        AdminCommands::VerifyResolver(args) => {
            crate::commands::verify_resolver::run(args)
        }
```

- [ ] **Step 4: Remove top-level `Commands::VerifyResolver`**

Edit `crates/cgn-cli/src/main.rs`:

Delete the variant definition (around line 73-75) and its dispatch arms (around line 125-127 and the fall-through arms at 153, 190).

- [ ] **Step 5: Run test to verify pass**

Run: `cargo build --workspace 2>&1 | tail -5`
Expected: build succeeds.

Run: `cargo test -p code-graph-nexus --test admin_verify_resolver_test`
Expected: 2 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/commands/admin/mod.rs \
        crates/cgn-cli/src/main.rs \
        crates/cgn-cli/tests/admin_verify_resolver_test.rs
git commit -m "refactor(admin): move verify-resolver from top-level to admin namespace"
```

---

### Task 3: Un-hide `shape_check` at top-level

**Files:**
- Modify: `crates/cgn-cli/src/main.rs` (~line 77-79: remove `#[command(hide = true)]`)
- Test: `crates/cgn-cli/tests/cli_help_surface_test.rs` (create)

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-cli/tests/cli_help_surface_test.rs`:

```rust
use std::process::Command;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

#[test]
fn top_level_help_contains_shape_check() {
    let output = Command::new(cgn_bin())
        .args(["--help"])
        .output()
        .expect("run cgn --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("shape_check") || stdout.contains("shape-check"),
        "expected shape_check in top-level --help, got: {stdout}"
    );
}

#[test]
fn top_level_help_excludes_admin_only_commands() {
    let output = Command::new(cgn_bin())
        .args(["--help"])
        .output()
        .expect("run cgn --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for hidden in ["verify-resolver", "verify_resolver"] {
        assert!(
            !stdout.contains(hidden),
            "{hidden} must not appear in top-level --help, got: {stdout}"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p code-graph-nexus --test cli_help_surface_test`
Expected: `top_level_help_contains_shape_check` fails because `shape_check` is currently hidden.

- [ ] **Step 3: Remove `hide = true` on `ShapeCheck`**

Edit `crates/cgn-cli/src/main.rs` — find `Commands::ShapeCheck` variant (around line 77-79). Remove the `#[command(hide = true)]` attribute. Update the doc comment to be user-facing:

```rust
    /// Detect drift between HTTP consumer access patterns and Route response shapes.
    ShapeCheck(commands::shape_check::ShapeCheckArgs),
```

- [ ] **Step 4: Run test to verify pass**

Run: `cargo build --workspace 2>&1 | tail -5`
Run: `cargo test -p code-graph-nexus --test cli_help_surface_test`
Expected: both tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cgn-cli/src/main.rs \
        crates/cgn-cli/tests/cli_help_surface_test.rs
git commit -m "feat(shape-check): un-hide as top-level agent-facing command"
```

---

### Task 4: Add `--route` filter to `shape_check`

**Files:**
- Modify: `crates/cgn-cli/src/commands/shape_check.rs`
- Test: `crates/cgn-cli/tests/shape_check_route_filter.rs` (create)

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-cli/tests/shape_check_route_filter.rs`:

```rust
//! Verify `cgn shape_check --route <path>` filters Fetches edges
//! by target Route path. No-match case prints helpful message.

use std::process::Command;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

#[test]
fn shape_check_help_lists_route_arg() {
    let output = Command::new(cgn_bin())
        .args(["shape_check", "--help"])
        .output()
        .expect("run cgn shape_check --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--route"),
        "expected --route arg in help, got: {stdout}"
    );
}

#[test]
fn shape_check_route_no_match_emits_helpful_message() {
    // Run shape_check against current repo with a clearly non-existent route.
    let output = Command::new(cgn_bin())
        .args(["shape_check", "--route", "/__nonexistent_route__"])
        .output()
        .expect("run cgn shape_check --route");
    // Exit code 0 (advisory), output contains helpful no-match phrase.
    assert!(output.status.success(), "shape_check should succeed even with no matches");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No routes match")
            || stdout.contains("no match")
            || stdout.is_empty(),
        "expected no-match message, got: {stdout}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p code-graph-nexus --test shape_check_route_filter`
Expected: `shape_check_help_lists_route_arg` fails — `--route` not recognized.

- [ ] **Step 3: Add `--route` to `ShapeCheckArgs`**

Edit `crates/cgn-cli/src/commands/shape_check.rs`:

Find `pub struct ShapeCheckArgs` and add the field:

```rust
#[derive(Args, Debug, Clone)]
pub struct ShapeCheckArgs {
    /// Repository root path (defaults to current directory).
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: text (default) | json | toon
    #[arg(long, default_value = "text")]
    pub format: Option<String>,

    /// Filter: only report drift for routes whose path matches this string.
    /// When None (default), all routes with extracted shape are checked.
    #[arg(long)]
    pub route: Option<String>,
}
```

- [ ] **Step 4: Filter edges by route path in `run()`**

In the same file, locate the body of `run()` where Fetches edges are iterated. Wrap each edge handling with a check:

```rust
// (existing) for edge in graph.edges_of_type(RelType::Fetches) { ... }
// Modify to:
let mut matched_count = 0usize;
for edge in graph.edges_of_type(RelType::Fetches) {
    // ... (existing target Route lookup logic) ...
    let target_route_path = /* extract path from target Route node */;

    if let Some(filter) = &args.route {
        if !target_route_path.contains(filter.as_str()) {
            continue;
        }
    }
    matched_count += 1;
    // ... (existing drift check + emit) ...
}

if args.route.is_some() && matched_count == 0 {
    eprintln!(
        "No routes match `{}` in the graph.",
        args.route.as_deref().unwrap_or("")
    );
}
```

(Adapt to the actual variable names in the existing file. The key invariants: filter by `args.route` substring match against target Route path; emit no-match message when filter is set and zero edges checked.)

- [ ] **Step 5: Run test to verify pass**

Run: `cargo build --workspace 2>&1 | tail -5`
Run: `cargo test -p code-graph-nexus --test shape_check_route_filter`
Expected: both tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/commands/shape_check.rs \
        crates/cgn-cli/tests/shape_check_route_filter.rs
git commit -m "feat(shape-check): add --route <path> filter for targeted drift detection"
```

---

### Task 5: Add `--format` arg to `admin mcp tools`

**Files:**
- Modify: `crates/cgn-cli/src/commands/mcp.rs`
- Test: `crates/cgn-cli/tests/admin_mcp_test.rs` (extend)

- [ ] **Step 1: Append failing test to `admin_mcp_test.rs`**

```rust
#[test]
fn admin_mcp_tools_json_format() {
    let output = Command::new(cgn_bin())
        .args(["admin", "mcp", "tools", "--format", "json"])
        .output()
        .expect("run cgn admin mcp tools --format json");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("output must be valid JSON");
    assert!(parsed.is_array() || parsed.get("tools").is_some(),
        "expected JSON array or {{tools: [...]}} object, got: {parsed}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p code-graph-nexus --test admin_mcp_test -- admin_mcp_tools_json_format`
Expected: fails because `--format` is not yet recognized.

- [ ] **Step 3: Update `McpAction::Tools` to accept `--format`**

Edit `crates/cgn-cli/src/commands/mcp.rs`:

```rust
#[derive(Subcommand, Debug, Clone)]
pub enum McpAction {
    /// Run stdio JSON-RPC MCP server.
    Serve,
    /// List tools that would be exposed by `serve`.
    Tools {
        /// Output format: text (default) | json | toon
        #[arg(long, default_value = "text")]
        format: String,
    },
}
```

Update `run()`:

```rust
pub fn run(args: McpArgs, root_cmd: Command) -> Result<(), CgnError> {
    let server =
        CgnMcpServer::new(&root_cmd).map_err(|e| CgnError::Output(format!("server init: {e}")))?;

    match args.action {
        McpAction::Tools { format } => {
            let tools = server.list_tools();
            match format.as_str() {
                "json" => println!("{}", serde_json::to_string_pretty(&tools)
                    .map_err(|e| CgnError::Output(format!("json: {e}")))?),
                "toon" => {
                    // Use existing toon emitter helper if available
                    let json = serde_json::to_value(&tools)
                        .map_err(|e| CgnError::Output(format!("toon: {e}")))?;
                    println!("{}", etoon::to_string(&json)
                        .map_err(|e| CgnError::Output(format!("toon emit: {e}")))?);
                }
                _ => {
                    // Existing text rendering: list name + description per tool
                    for tool in tools {
                        println!("{}\t{}", tool.name, tool.description);
                    }
                }
            }
            Ok(())
        }
        McpAction::Serve => serve_stdio(server),
    }
}
```

(Adapt: check whether `etoon` crate is in `Cargo.toml`, or reuse existing `output::emit` helper. The intent: dispatch by format string with default text behavior preserved.)

- [ ] **Step 4: Run test to verify pass**

Run: `cargo build --workspace 2>&1 | tail -5`
Run: `cargo test -p code-graph-nexus --test admin_mcp_test`
Expected: all admin_mcp_test cases pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cgn-cli/src/commands/mcp.rs \
        crates/cgn-cli/tests/admin_mcp_test.rs
git commit -m "feat(mcp): add --format <json|toon|text> to admin mcp tools"
```

---

### Task 6: Skeleton `cgn diff` command (DiffArgs + dispatch wired with stub)

**Files:**
- Create: `crates/cgn-cli/src/commands/diff/mod.rs`
- Modify: `crates/cgn-cli/src/commands/mod.rs` (add `pub mod diff;`)
- Modify: `crates/cgn-cli/src/main.rs` (add `Commands::Diff(diff::DiffArgs)` variant + dispatch)
- Test: `crates/cgn-cli/tests/diff_baseline_resolve_test.rs` (create with stub test)

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-cli/tests/diff_baseline_resolve_test.rs`:

```rust
//! Verify `cgn diff` CLI surface: required args, section enum, baseline rejection.

use std::process::Command;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

#[test]
fn diff_requires_section_and_baseline() {
    let output = Command::new(cgn_bin())
        .args(["diff"])
        .output()
        .expect("run cgn diff");
    assert!(!output.status.success(), "diff without args must reject");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--section") || stderr.contains("section"),
        "missing-section hint expected, got stderr: {stderr}"
    );
    assert!(
        stderr.contains("--baseline") || stderr.contains("baseline"),
        "missing-baseline hint expected, got stderr: {stderr}"
    );
}

#[test]
fn diff_help_lists_section_choices() {
    let output = Command::new(cgn_bin())
        .args(["diff", "--help"])
        .output()
        .expect("run cgn diff --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for word in ["bindings", "routes", "contracts", "all"] {
        assert!(
            stdout.contains(word),
            "expected `{word}` in --help possible values, got: {stdout}"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p code-graph-nexus --test diff_baseline_resolve_test`
Expected: both tests fail with "unrecognized subcommand 'diff'".

- [ ] **Step 3: Create `commands/diff/mod.rs` with `DiffArgs` skeleton**

Create `crates/cgn-cli/src/commands/diff/mod.rs`:

```rust
//! `cgn diff` — generalized graph-level cross-commit diff command.
//!
//! Compares two git refs and emits structural changes per requested section
//! (bindings / routes / contracts / all). See spec
//! docs/superpowers/specs/2026-05-16-tier-b-surface-and-diff-design.md §5.

use clap::{Args, ValueEnum};
use cgn_core::CgnError;

pub mod baseline;
pub mod bindings;
pub mod contracts;
pub mod git_guard;
pub mod output;
pub mod routes;

/// Section of the graph to diff. `All` = bindings + routes + contracts.
#[derive(ValueEnum, Clone, Debug, PartialEq, Eq, Hash)]
#[value(rename_all = "lowercase")]
pub enum DiffSection {
    Bindings,
    Routes,
    Contracts,
    All,
}

#[derive(Args, Debug, Clone)]
pub struct DiffArgs {
    /// Comma-separated section(s) to diff: bindings, routes, contracts, or all.
    #[arg(long, value_delimiter = ',', required = true)]
    pub section: Vec<DiffSection>,

    /// Git ref to compare against: branch / tag / commit SHA / HEAD~N / PR/<n>. No default.
    #[arg(long, required = true)]
    pub baseline: String,

    /// Output format: text (default) | json | toon
    #[arg(long, default_value = "text")]
    pub format: String,

    /// List every change (text format only). Default truncates to top-10 per section.
    #[arg(long, default_value_t = false)]
    pub verbose: bool,

    /// Repository root path (defaults to current directory).
    #[arg(long)]
    pub repo: Option<String>,
}

pub fn run(args: DiffArgs) -> Result<(), CgnError> {
    // Stub for Task 6 — implementation lands in Tasks 7-14.
    let _ = args;
    Err(CgnError::Output(
        "cgn diff not yet implemented; tracking issue: tier-b plan task 6".into(),
    ))
}
```

- [ ] **Step 4: Register `diff` module + add `Commands::Diff` dispatch**

Edit `crates/cgn-cli/src/commands/mod.rs`:

```rust
pub mod admin;
pub mod contracts;
pub mod coverage;
pub mod cypher;
pub mod diff;   // <-- add
pub mod format;
// ... rest unchanged
```

Edit `crates/cgn-cli/src/main.rs` — add variant after `Contracts`:

```rust
    /// Cross-repo API contracts inventory (routes / queue / RPC)
    Contracts(commands::contracts::ContractsArgs),

    /// Cross-commit graph diff (bindings / routes / contracts).
    Diff(commands::diff::DiffArgs),
```

In the no-graph dispatch match (before "fall through to graph-loading path"):

```rust
        Commands::Diff(args) => run_no_graph!(commands::diff::run(args.clone())),
```

In the fall-through arms (the two `Commands::Coverage(_) | Commands::Contracts(_) | ...` matches), add `| Commands::Diff(_)` to keep them ignoring the graph path.

- [ ] **Step 5: Run test to verify pass**

Run: `cargo build --workspace 2>&1 | tail -5`
Expected: build succeeds (stub run returns error, but dispatch wired).

Run: `cargo test -p code-graph-nexus --test diff_baseline_resolve_test`
Expected: both tests pass — clap rejects missing args with hint; `--help` lists section choices.

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/commands/diff/mod.rs \
        crates/cgn-cli/src/commands/mod.rs \
        crates/cgn-cli/src/main.rs \
        crates/cgn-cli/tests/diff_baseline_resolve_test.rs
git commit -m "feat(diff): wire DiffArgs + dispatch skeleton (stub run)"
```

---

### Task 7: Implement baseline ref resolution

**Files:**
- Create: `crates/cgn-cli/src/commands/diff/baseline.rs`
- Modify: `crates/cgn-cli/tests/diff_baseline_resolve_test.rs` (extend)

- [ ] **Step 1: Append failing tests**

Append to `crates/cgn-cli/tests/diff_baseline_resolve_test.rs`:

```rust
use std::process::Command;

// Note: tests assume the working repo has at least one commit and `origin/main` available.
// Tests that need PR/<n> resolution skip on CI machines without gh CLI.

#[test]
fn diff_baseline_invalid_ref_errors_with_hint() {
    let output = Command::new(env!("CARGO_BIN_EXE_cgn"))
        .args(["diff", "--section", "bindings", "--baseline", "definitely-no-such-ref"])
        .output()
        .expect("run cgn diff");
    assert!(!output.status.success(), "invalid ref must error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot resolve") || stderr.contains("not found")
            || stderr.contains("unknown") || stderr.contains("baseline"),
        "expected unresolvable-ref hint, got: {stderr}"
    );
}

#[test]
fn diff_baseline_pr_form_calls_gh() {
    // Skip when gh is not installed.
    let gh_check = Command::new("gh").arg("--version").output();
    if gh_check.is_err() || !gh_check.unwrap().status.success() {
        eprintln!("skipping: gh CLI not installed");
        return;
    }
    // Use a known non-existent PR; expectation: clean error from gh, surfaced by cgn.
    let output = Command::new(env!("CARGO_BIN_EXE_cgn"))
        .args(["diff", "--section", "bindings", "--baseline", "PR/9999999"])
        .output()
        .expect("run cgn diff");
    assert!(!output.status.success(), "non-existent PR must error");
}
```

- [ ] **Step 2: Run test to verify the new tests fail**

Run: `cargo test -p code-graph-nexus --test diff_baseline_resolve_test`
Expected: `diff_baseline_invalid_ref_errors_with_hint` fails because stub `run` always returns the same placeholder error without honoring the ref.

- [ ] **Step 3: Implement baseline resolver**

Create `crates/cgn-cli/src/commands/diff/baseline.rs`:

```rust
//! Resolve a `--baseline <ref>` value to a concrete commit SHA.
//!
//! Accepted forms:
//! - Branch:       `main`, `origin/main`
//! - Tag:          `v1.2.0`
//! - Commit SHA:   `a8b2f54` (short or full)
//! - Relative:     `HEAD~5`
//! - PR number:    `PR/13` (requires `gh` CLI authenticated to the repo)

use cgn_core::CgnError;
use std::process::Command;

/// Resolve `ref_str` to a 40-char commit SHA inside the given repo dir.
pub fn resolve(ref_str: &str, repo_dir: &std::path::Path) -> Result<String, CgnError> {
    if let Some(pr_num) = ref_str.strip_prefix("PR/") {
        return resolve_pr(pr_num, repo_dir);
    }
    resolve_via_git(ref_str, repo_dir)
}

fn resolve_via_git(ref_str: &str, repo_dir: &std::path::Path) -> Result<String, CgnError> {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", &format!("{ref_str}^{{commit}}")])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| CgnError::Output(format!("git rev-parse failed to spawn: {e}")))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(CgnError::Output(format!(
            "cannot resolve baseline `{ref_str}`: {}\n\
             accepted: branch / tag / commit SHA / HEAD~N / PR/<n>",
            stderr.trim()
        )));
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.len() < 7 {
        return Err(CgnError::Output(format!(
            "git rev-parse returned suspect output for `{ref_str}`: `{sha}`"
        )));
    }
    Ok(sha)
}

fn resolve_pr(pr_num: &str, repo_dir: &std::path::Path) -> Result<String, CgnError> {
    if !pr_num.chars().all(|c| c.is_ascii_digit()) {
        return Err(CgnError::Output(format!(
            "PR number must be numeric, got `{pr_num}`"
        )));
    }
    // Require gh CLI.
    let out = Command::new("gh")
        .args(["pr", "view", pr_num, "--json", "baseRefOid", "--jq", ".baseRefOid"])
        .current_dir(repo_dir)
        .output()
        .map_err(|_| {
            CgnError::Output(
                "gh CLI not found; install gh or pass commit SHA directly".into(),
            )
        })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(CgnError::Output(format!(
            "cannot resolve PR/{pr_num}: {}",
            stderr.trim()
        )));
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        return Err(CgnError::Output(format!(
            "gh pr view PR/{pr_num} returned empty baseRefOid"
        )));
    }
    Ok(sha)
}
```

- [ ] **Step 4: Wire baseline resolution into `DiffArgs::run`**

Edit `crates/cgn-cli/src/commands/diff/mod.rs` — replace stub:

```rust
pub fn run(args: DiffArgs) -> Result<(), CgnError> {
    let repo_dir = args
        .repo
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));

    let baseline_sha = baseline::resolve(&args.baseline, &repo_dir)?;

    // Tasks 8+: stash + checkout baseline_sha, run analyzer, compare.
    // For now, surface that resolution worked.
    return Err(CgnError::Output(format!(
        "baseline resolved to {baseline_sha}; section diff not yet implemented"
    )));
}
```

- [ ] **Step 5: Run tests**

Run: `cargo build --workspace 2>&1 | tail -5`
Run: `cargo test -p code-graph-nexus --test diff_baseline_resolve_test`
Expected: all 4 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/commands/diff/baseline.rs \
        crates/cgn-cli/src/commands/diff/mod.rs \
        crates/cgn-cli/tests/diff_baseline_resolve_test.rs
git commit -m "feat(diff): resolve --baseline ref to commit SHA (branch/tag/SHA/PR)"
```

---

### Task 8: Git stash + checkout RAII guard

**Files:**
- Create: `crates/cgn-cli/src/commands/diff/git_guard.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/cgn-cli/tests/diff_baseline_resolve_test.rs`:

```rust
#[test]
fn git_guard_restores_branch_on_drop() {
    use std::env;

    // Capture current branch HEAD ref.
    let before = Command::new("git").args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(env::current_dir().unwrap())
        .output()
        .expect("git symbolic-ref");
    let before_branch = String::from_utf8_lossy(&before.stdout).trim().to_string();
    if before_branch.is_empty() {
        eprintln!("skipping: HEAD is detached");
        return;
    }

    // Resolve a real baseline (HEAD itself for trivial round-trip).
    let baseline_sha = {
        let out = Command::new("git").args(["rev-parse", "HEAD"])
            .output().expect("git rev-parse").stdout;
        String::from_utf8_lossy(&out).trim().to_string()
    };

    // Use the cgn binary to run a stub that exercises guard then restores.
    // We probe via a no-op diff command: it should succeed-or-error, but always
    // restore HEAD.
    let _ = Command::new(env!("CARGO_BIN_EXE_cgn"))
        .args(["diff", "--section", "bindings", "--baseline", &baseline_sha])
        .output();

    let after = Command::new("git").args(["symbolic-ref", "--short", "HEAD"])
        .output().expect("git symbolic-ref");
    let after_branch = String::from_utf8_lossy(&after.stdout).trim().to_string();
    assert_eq!(before_branch, after_branch, "branch must be restored after diff");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p code-graph-nexus --test diff_baseline_resolve_test -- git_guard_restores_branch_on_drop`
Expected: may pass trivially since diff currently returns an error before any checkout. But once Task 9 adds checkout, this guards against regression. Confirm the test framework records expected outcome.

- [ ] **Step 3: Implement `GitGuard` RAII type**

Create `crates/cgn-cli/src/commands/diff/git_guard.rs`:

```rust
//! RAII git workspace guard for `cgn diff`.
//!
//! On `enter`:
//!   1. Stash dirty tree (if any), recording whether stash was created.
//!   2. Detach HEAD to target SHA.
//!
//! On drop:
//!   3. Checkout the original ref.
//!   4. `git stash pop` if a stash was created in step 1.
//!
//! Errors during drop are logged to stderr (we cannot return from Drop).

use cgn_core::CgnError;
use std::path::PathBuf;
use std::process::Command;

pub struct GitGuard {
    repo_dir: PathBuf,
    original_ref: String,
    stash_created: bool,
}

impl GitGuard {
    /// Detach HEAD to `target_sha`. Stashes dirty tree if any.
    pub fn enter(repo_dir: &std::path::Path, target_sha: &str) -> Result<Self, CgnError> {
        // Capture original HEAD: branch name if on a branch, else SHA (detached).
        let original_ref = current_head_ref(repo_dir)?;

        // Stash if dirty.
        let stash_created = stash_if_dirty(repo_dir)?;

        // Detach HEAD.
        let out = Command::new("git")
            .args(["checkout", "--detach", target_sha])
            .current_dir(repo_dir)
            .output()
            .map_err(|e| CgnError::Output(format!("git checkout failed to spawn: {e}")))?;
        if !out.status.success() {
            // Best-effort cleanup before erroring.
            let mut guard = GitGuard {
                repo_dir: repo_dir.to_path_buf(),
                original_ref,
                stash_created,
            };
            guard.restore_inner();
            std::mem::forget(guard);
            return Err(CgnError::Output(format!(
                "git checkout {target_sha} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }

        Ok(GitGuard {
            repo_dir: repo_dir.to_path_buf(),
            original_ref,
            stash_created,
        })
    }

    fn restore_inner(&mut self) {
        let _ = Command::new("git")
            .args(["checkout", &self.original_ref])
            .current_dir(&self.repo_dir)
            .output();
        if self.stash_created {
            let _ = Command::new("git")
                .args(["stash", "pop"])
                .current_dir(&self.repo_dir)
                .output();
        }
    }
}

impl Drop for GitGuard {
    fn drop(&mut self) {
        self.restore_inner();
    }
}

fn current_head_ref(repo_dir: &std::path::Path) -> Result<String, CgnError> {
    // Try symbolic-ref (branch).
    let out = Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| CgnError::Output(format!("git symbolic-ref failed: {e}")))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    // Detached HEAD: capture SHA.
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| CgnError::Output(format!("git rev-parse HEAD failed: {e}")))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn stash_if_dirty(repo_dir: &std::path::Path) -> Result<bool, CgnError> {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| CgnError::Output(format!("git status failed: {e}")))?;
    if out.stdout.is_empty() {
        return Ok(false);
    }
    let stash = Command::new("git")
        .args(["stash", "push", "-u", "-m", "cgn-diff-auto-stash"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| CgnError::Output(format!("git stash failed: {e}")))?;
    if !stash.status.success() {
        return Err(CgnError::Output(format!(
            "git stash push failed: {}",
            String::from_utf8_lossy(&stash.stderr).trim()
        )));
    }
    Ok(true)
}
```

- [ ] **Step 4: Use guard in `DiffArgs::run`**

Edit `crates/cgn-cli/src/commands/diff/mod.rs`:

```rust
pub fn run(args: DiffArgs) -> Result<(), CgnError> {
    let repo_dir = args
        .repo
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));

    let baseline_sha = baseline::resolve(&args.baseline, &repo_dir)?;
    let current_sha = baseline::resolve("HEAD", &repo_dir)?;

    // Build baseline graph data via temp checkout.
    let baseline_data = {
        let _guard = git_guard::GitGuard::enter(&repo_dir, &baseline_sha)?;
        snapshot_sections(&repo_dir, &args.section)?
    }; // _guard dropped here, restores HEAD

    let current_data = snapshot_sections(&repo_dir, &args.section)?;
    let _ = current_sha;
    let _ = baseline_data;
    let _ = current_data;
    // Section diff lands in Tasks 9-12.
    Err(CgnError::Output(
        "section diff not yet implemented".into(),
    ))
}

fn snapshot_sections(_repo_dir: &std::path::Path, _sections: &[DiffSection])
    -> Result<SectionSnapshot, CgnError>
{
    Ok(SectionSnapshot::default())
}

#[derive(Default)]
pub(crate) struct SectionSnapshot {
    pub bindings: Vec<serde_json::Value>,
    pub routes: Vec<serde_json::Value>,
    pub contracts: Vec<serde_json::Value>,
}
```

- [ ] **Step 5: Run tests**

Run: `cargo build --workspace 2>&1 | tail -5`
Run: `cargo test -p code-graph-nexus --test diff_baseline_resolve_test`
Expected: all tests pass, including `git_guard_restores_branch_on_drop`.

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/commands/diff/git_guard.rs \
        crates/cgn-cli/src/commands/diff/mod.rs \
        crates/cgn-cli/tests/diff_baseline_resolve_test.rs
git commit -m "feat(diff): GitGuard RAII for stash + detach-checkout + restore"
```

---

### Task 9: `bindings` section diff

**Files:**
- Implement: `crates/cgn-cli/src/commands/diff/bindings.rs`
- Modify: `crates/cgn-cli/src/commands/diff/mod.rs` (call into bindings)
- Test: `crates/cgn-cli/tests/diff_bindings_test.rs` (create)

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-cli/tests/diff_bindings_test.rs`:

```rust
//! Verify `cgn diff --section bindings --baseline <ref>` returns
//! resolver decision changes between two refs.

use std::process::Command;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

#[test]
fn diff_bindings_against_head_yields_empty() {
    // Diff HEAD vs HEAD: no resolver decisions changed.
    let head_sha = {
        let out = Command::new("git").args(["rev-parse", "HEAD"]).output().unwrap().stdout;
        String::from_utf8_lossy(&out).trim().to_string()
    };
    let output = Command::new(cgn_bin())
        .args(["diff", "--section", "bindings", "--baseline", &head_sha,
               "--format", "json"])
        .output()
        .expect("run cgn diff bindings");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}; stdout was: {stdout}"));
    // bindings section should have all-empty arrays.
    let bindings = &parsed["sections"]["bindings"];
    for key in ["new_resolutions", "tier_changes", "target_changes", "removed"] {
        let arr = bindings[key].as_array()
            .unwrap_or_else(|| panic!("missing {key}"));
        assert!(arr.is_empty(), "{key} should be empty for HEAD vs HEAD; got {arr:?}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p code-graph-nexus --test diff_bindings_test`
Expected: fails because `run()` still returns "section diff not yet implemented".

- [ ] **Step 3: Implement `bindings.rs`**

Create `crates/cgn-cli/src/commands/diff/bindings.rs`:

```rust
//! `bindings` section: compare per-binding resolver decisions across two
//! commits. Each binding is keyed by `(src_file, symbol_name)`.

use cgn_core::CgnError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct BindingDecision {
    pub src_file: String,
    pub name: String,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub target_file: Option<String>,
    #[serde(default)]
    pub target_id: Option<u32>,
    #[serde(default)]
    pub confidence: Option<f64>,
}

#[derive(Debug, Serialize, Default)]
pub struct BindingsDiff {
    pub new_resolutions: Vec<BindingChange>,
    pub tier_changes: Vec<BindingChange>,
    pub target_changes: Vec<BindingChange>,
    pub removed: Vec<BindingChange>,
}

#[derive(Debug, Serialize, Clone)]
pub struct BindingChange {
    pub src_file: String,
    pub name: String,
    pub before: Option<BindingDecision>,
    pub after: Option<BindingDecision>,
}

/// Dump resolver decisions for the working tree into `out_path` as JSONL.
pub fn dump(repo_dir: &Path, out_path: &Path) -> Result<(), CgnError> {
    let out = Command::new(env!("CARGO_BIN_EXE_cgn"))
        .args([
            "admin", "index",
            "--dump-resolver", out_path.to_str().expect("path utf8"),
        ])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| CgnError::Output(format!("cgn admin index spawn: {e}")))?;
    if !out.status.success() {
        return Err(CgnError::Output(format!(
            "cgn admin index --dump-resolver failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// Parse a JSONL file of `BindingDecision` records into a `(src_file, name) → decision` map.
pub fn load_jsonl(path: &Path) -> Result<HashMap<(String, String), BindingDecision>, CgnError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| CgnError::Output(format!("read {}: {e}", path.display())))?;
    let mut map = HashMap::new();
    for (idx, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let dec: BindingDecision = serde_json::from_str(line).map_err(|e| {
            CgnError::Output(format!("JSONL line {} parse: {e}", idx + 1))
        })?;
        map.insert((dec.src_file.clone(), dec.name.clone()), dec);
    }
    Ok(map)
}

/// Compare baseline vs current binding decisions and bucket the diff.
pub fn diff(
    baseline: &HashMap<(String, String), BindingDecision>,
    current: &HashMap<(String, String), BindingDecision>,
) -> BindingsDiff {
    let mut out = BindingsDiff::default();
    let mut keys: Vec<&(String, String)> = baseline.keys().chain(current.keys()).collect();
    keys.sort();
    keys.dedup();

    for key in keys {
        let b = baseline.get(key);
        let c = current.get(key);
        let change = BindingChange {
            src_file: key.0.clone(),
            name: key.1.clone(),
            before: b.cloned(),
            after: c.cloned(),
        };
        match (b, c) {
            (None, Some(_)) => out.new_resolutions.push(change),
            (Some(_), None) => out.removed.push(change),
            (Some(b), Some(c)) => {
                if b.target_file != c.target_file {
                    out.target_changes.push(change);
                } else if b.tier != c.tier {
                    out.tier_changes.push(change);
                }
                // else: unchanged, drop.
            }
            (None, None) => {}
        }
    }
    out
}

pub fn to_json(diff: &BindingsDiff) -> Value {
    serde_json::to_value(diff).unwrap_or(Value::Null)
}
```

- [ ] **Step 4: Integrate bindings into `DiffArgs::run`**

Edit `crates/cgn-cli/src/commands/diff/mod.rs`:

```rust
use crate::commands::diff::bindings::{dump as dump_bindings, load_jsonl, BindingsDiff};

pub fn run(args: DiffArgs) -> Result<(), CgnError> {
    let repo_dir = args.repo.as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));

    let baseline_sha = baseline::resolve(&args.baseline, &repo_dir)?;
    let want_bindings = args.section.iter().any(|s| matches!(s, DiffSection::Bindings | DiffSection::All));
    let want_routes = args.section.iter().any(|s| matches!(s, DiffSection::Routes | DiffSection::All));
    let want_contracts = args.section.iter().any(|s| matches!(s, DiffSection::Contracts | DiffSection::All));

    let mut bindings_diff: Option<BindingsDiff> = None;

    if want_bindings {
        let baseline_jsonl = std::env::temp_dir().join(format!("cgn-diff-bindings-{baseline_sha}.jsonl"));
        let current_jsonl  = std::env::temp_dir().join(format!("cgn-diff-bindings-current-{}.jsonl", std::process::id()));

        {
            let _guard = git_guard::GitGuard::enter(&repo_dir, &baseline_sha)?;
            dump_bindings(&repo_dir, &baseline_jsonl)?;
        }
        dump_bindings(&repo_dir, &current_jsonl)?;

        let baseline_map = load_jsonl(&baseline_jsonl)?;
        let current_map = load_jsonl(&current_jsonl)?;
        bindings_diff = Some(bindings::diff(&baseline_map, &current_map));

        let _ = std::fs::remove_file(&baseline_jsonl);
        let _ = std::fs::remove_file(&current_jsonl);
    }

    let _ = (want_routes, want_contracts); // Tasks 10-11.

    // Emit (text/json/toon) — final formatter wired in Task 13.
    if args.format == "json" {
        let mut envelope = serde_json::json!({
            "baseline": {"ref": args.baseline, "sha": baseline_sha},
            "current": {"ref": "HEAD"},
            "sections": {}
        });
        if let Some(bd) = &bindings_diff {
            envelope["sections"]["bindings"] = bindings::to_json(bd);
        }
        println!("{}", serde_json::to_string_pretty(&envelope)
            .map_err(|e| CgnError::Output(format!("json emit: {e}")))?);
    } else {
        // text fallback for now; Task 13 formats nicely.
        println!("Bindings diff baseline=`{}` (sha={})", args.baseline, baseline_sha);
        if let Some(bd) = &bindings_diff {
            println!("  new_resolutions: {}", bd.new_resolutions.len());
            println!("  tier_changes:    {}", bd.tier_changes.len());
            println!("  target_changes:  {}", bd.target_changes.len());
            println!("  removed:         {}", bd.removed.len());
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Run tests**

Run: `cargo build --workspace 2>&1 | tail -5`
Run: `cargo test -p code-graph-nexus --test diff_bindings_test`
Expected: pass — HEAD vs HEAD yields empty arrays in all 4 categories.

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/commands/diff/bindings.rs \
        crates/cgn-cli/src/commands/diff/mod.rs \
        crates/cgn-cli/tests/diff_bindings_test.rs
git commit -m "feat(diff): bindings section — compare resolver decisions across refs"
```

---

### Task 10: `routes` section diff

**Files:**
- Create: `crates/cgn-cli/src/commands/diff/routes.rs`
- Modify: `crates/cgn-cli/src/commands/diff/mod.rs`
- Test: `crates/cgn-cli/tests/diff_routes_test.rs`

- [ ] **Step 1: Write the failing test**

```rust
use std::process::Command;
fn cgn_bin() -> &'static str { env!("CARGO_BIN_EXE_cgn") }

#[test]
fn diff_routes_head_vs_head_empty() {
    let head_sha = {
        let out = Command::new("git").args(["rev-parse", "HEAD"]).output().unwrap().stdout;
        String::from_utf8_lossy(&out).trim().to_string()
    };
    let output = Command::new(cgn_bin())
        .args(["diff", "--section", "routes", "--baseline", &head_sha, "--format", "json"])
        .output()
        .expect("run cgn diff routes");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let routes = &parsed["sections"]["routes"];
    for key in ["added", "removed", "modified"] {
        let arr = routes[key].as_array().unwrap_or_else(|| panic!("missing {key}"));
        assert!(arr.is_empty(), "{key} should be empty: {arr:?}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p code-graph-nexus --test diff_routes_test`
Expected: fails — routes section not in JSON envelope.

- [ ] **Step 3: Implement `routes.rs`**

Create `crates/cgn-cli/src/commands/diff/routes.rs`:

```rust
//! `routes` section: compare Route nodes between two graph snapshots.

use cgn_core::graph::ZeroCopyGraph;
use cgn_core::CgnError;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct RouteEntry {
    pub method: String,
    pub path: String,
    pub handler_file: String,
    pub handler_line: u32,
    pub response_shape_keys: Vec<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct RoutesDiff {
    pub added: Vec<RouteEntry>,
    pub removed: Vec<RouteEntry>,
    pub modified: Vec<RouteChange>,
}

#[derive(Debug, Serialize)]
pub struct RouteChange {
    pub before: RouteEntry,
    pub after: RouteEntry,
}

/// Extract all routes from a graph file (rkyv mmap).
pub fn extract(graph_path: &Path) -> Result<Vec<RouteEntry>, CgnError> {
    let bytes = std::fs::read(graph_path)
        .map_err(|e| CgnError::Output(format!("read graph: {e}")))?;
    let graph = ZeroCopyGraph::from_bytes(&bytes)
        .map_err(|e| CgnError::Output(format!("graph load: {e}")))?;
    // Iterate Route-kind nodes. Adapt to actual graph API.
    let mut routes = Vec::new();
    for node in graph.nodes_of_kind(cgn_core::graph::NodeKind::Route) {
        let path = node.name.to_string(); // adapt: route path stored in name
        let (method, route_path) = parse_method_path(&path);
        routes.push(RouteEntry {
            method,
            path: route_path,
            handler_file: node.file_path.to_string(),
            handler_line: node.line,
            response_shape_keys: Vec::new(), // populate from RouteShape if available
        });
    }
    Ok(routes)
}

fn parse_method_path(s: &str) -> (String, String) {
    // cgn stores route names as "GET /api/users" — split on first space.
    let mut parts = s.splitn(2, ' ');
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    (method, path)
}

pub fn diff(baseline: &[RouteEntry], current: &[RouteEntry]) -> RoutesDiff {
    let key = |r: &RouteEntry| (r.method.clone(), r.path.clone());
    let baseline_map: HashMap<_, _> = baseline.iter().map(|r| (key(r), r)).collect();
    let current_map: HashMap<_, _> = current.iter().map(|r| (key(r), r)).collect();

    let mut out = RoutesDiff::default();
    for (k, b) in &baseline_map {
        match current_map.get(k) {
            None => out.removed.push((*b).clone()),
            Some(c) if c != b => out.modified.push(RouteChange { before: (*b).clone(), after: (*c).clone() }),
            _ => {}
        }
    }
    for (k, c) in &current_map {
        if !baseline_map.contains_key(k) {
            out.added.push((*c).clone());
        }
    }
    out
}

pub fn to_json(diff: &RoutesDiff) -> serde_json::Value {
    serde_json::to_value(diff).unwrap_or(serde_json::Value::Null)
}
```

(Adapt `ZeroCopyGraph::nodes_of_kind` and `parse_method_path` to actual cgn Graph API. If `graph.nodes_of_kind` doesn't exist, iterate `graph.nodes` and filter `kind == NodeKind::Route`.)

- [ ] **Step 4: Integrate into `DiffArgs::run`**

In `crates/cgn-cli/src/commands/diff/mod.rs`, add a routes branch after the bindings handling:

```rust
let mut routes_diff: Option<routes::RoutesDiff> = None;
if want_routes {
    let baseline_graph = std::env::temp_dir().join(format!("cgn-diff-routes-baseline-{baseline_sha}.bin"));
    let current_graph_path = resolve_current_graph_bin(&repo_dir)?;

    {
        let _guard = git_guard::GitGuard::enter(&repo_dir, &baseline_sha)?;
        let bg = resolve_current_graph_bin(&repo_dir)?;
        std::fs::copy(&bg, &baseline_graph)
            .map_err(|e| CgnError::Output(format!("copy baseline graph: {e}")))?;
    }
    let baseline_routes = routes::extract(&baseline_graph)?;
    let current_routes  = routes::extract(&current_graph_path)?;
    routes_diff = Some(routes::diff(&baseline_routes, &current_routes));
    let _ = std::fs::remove_file(&baseline_graph);
}

if args.format == "json" {
    // ... extend envelope with sections.routes = routes::to_json(...) ...
    if let Some(rd) = &routes_diff {
        envelope["sections"]["routes"] = routes::to_json(rd);
    }
}
```

Add helper:

```rust
fn resolve_current_graph_bin(repo_dir: &std::path::Path) -> Result<std::path::PathBuf, CgnError> {
    // Reuse existing graph_path::resolve logic from cgn
    // Or call cgn admin index --dump-graph if needed
    let path = repo_dir.join(".gitnexus-rs/graph.bin");
    if !path.exists() {
        return Err(CgnError::Output(format!(
            "graph.bin not found at {}; run `cgn admin index` first",
            path.display()
        )));
    }
    Ok(path)
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p code-graph-nexus --test diff_routes_test`
Expected: pass — empty diff for HEAD vs HEAD.

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/commands/diff/routes.rs \
        crates/cgn-cli/src/commands/diff/mod.rs \
        crates/cgn-cli/tests/diff_routes_test.rs
git commit -m "feat(diff): routes section — Route node added/removed/modified"
```

---

### Task 11: `contracts` section diff

**Files:**
- Create: `crates/cgn-cli/src/commands/diff/contracts.rs`
- Modify: `crates/cgn-cli/src/commands/diff/mod.rs`
- Test: `crates/cgn-cli/tests/diff_contracts_test.rs`

- [ ] **Step 1: Write the failing test**

```rust
use std::process::Command;
fn cgn_bin() -> &'static str { env!("CARGO_BIN_EXE_cgn") }

#[test]
fn diff_contracts_head_vs_head_empty() {
    let head_sha = {
        let out = Command::new("git").args(["rev-parse", "HEAD"]).output().unwrap().stdout;
        String::from_utf8_lossy(&out).trim().to_string()
    };
    let output = Command::new(cgn_bin())
        .args(["diff", "--section", "contracts", "--baseline", &head_sha, "--format", "json"])
        .output()
        .expect("run cgn diff contracts");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let contracts = &parsed["sections"]["contracts"];
    for key in ["added", "removed", "modified"] {
        assert!(
            contracts[key].as_array().expect("array").is_empty(),
            "{key} should be empty: {contracts:?}"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p code-graph-nexus --test diff_contracts_test`
Expected: fails — contracts section not in JSON envelope.

- [ ] **Step 3: Implement `contracts.rs`**

Create `crates/cgn-cli/src/commands/diff/contracts.rs`:

```rust
//! `contracts` section: compare cross-repo contract entries (RPC / queue / Fetches
//! response shapes) between two graph snapshots. Mirrors `cgn contracts` extraction.

use cgn_core::graph::ZeroCopyGraph;
use cgn_core::CgnError;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct ContractEntry {
    pub kind: String,        // "rpc" | "queue" | "fetch"
    pub identifier: String,  // path / queue name / RPC method name
    pub schema_keys: Vec<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct ContractsDiff {
    pub added: Vec<ContractEntry>,
    pub removed: Vec<ContractEntry>,
    pub modified: Vec<ContractChange>,
}

#[derive(Debug, Serialize)]
pub struct ContractChange {
    pub before: ContractEntry,
    pub after: ContractEntry,
}

pub fn extract(graph_path: &Path) -> Result<Vec<ContractEntry>, CgnError> {
    let bytes = std::fs::read(graph_path)
        .map_err(|e| CgnError::Output(format!("read graph: {e}")))?;
    let graph = ZeroCopyGraph::from_bytes(&bytes)
        .map_err(|e| CgnError::Output(format!("graph load: {e}")))?;

    // Adapt to actual contract-extraction logic in cgn contracts command.
    // Skeleton iterates Fetches edges and emits the request/response shape per route.
    let mut out = Vec::new();
    for edge in graph.edges() {
        if edge.rel_type != cgn_core::graph::RelType::Fetches { continue; }
        let target = graph.node(edge.target);
        let entry = ContractEntry {
            kind: "fetch".into(),
            identifier: format!("{} {}", "FETCH", target.name),
            schema_keys: Vec::new(), // populate from RouteShape if available
        };
        out.push(entry);
    }
    Ok(out)
}

pub fn diff(baseline: &[ContractEntry], current: &[ContractEntry]) -> ContractsDiff {
    let key = |c: &ContractEntry| (c.kind.clone(), c.identifier.clone());
    let baseline_map: HashMap<_, _> = baseline.iter().map(|c| (key(c), c)).collect();
    let current_map: HashMap<_, _> = current.iter().map(|c| (key(c), c)).collect();
    let mut out = ContractsDiff::default();
    for (k, b) in &baseline_map {
        match current_map.get(k) {
            None => out.removed.push((*b).clone()),
            Some(c) if c != b => out.modified.push(ContractChange {
                before: (*b).clone(), after: (*c).clone(),
            }),
            _ => {}
        }
    }
    for (k, c) in &current_map {
        if !baseline_map.contains_key(k) {
            out.added.push((*c).clone());
        }
    }
    out
}

pub fn to_json(diff: &ContractsDiff) -> serde_json::Value {
    serde_json::to_value(diff).unwrap_or(serde_json::Value::Null)
}
```

- [ ] **Step 4: Integrate into `DiffArgs::run`**

Mirror the `routes` integration: stash + checkout baseline + copy graph.bin + extract + diff. Add to JSON envelope under `sections.contracts`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p code-graph-nexus --test diff_contracts_test`
Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/commands/diff/contracts.rs \
        crates/cgn-cli/src/commands/diff/mod.rs \
        crates/cgn-cli/tests/diff_contracts_test.rs
git commit -m "feat(diff): contracts section — cross-repo contract added/removed/modified"
```

---

### Task 12: Output formatters (text + json + toon)

**Files:**
- Create: `crates/cgn-cli/src/commands/diff/output.rs`
- Modify: `crates/cgn-cli/src/commands/diff/mod.rs` (replace inline emit with formatter)
- Test: `crates/cgn-cli/tests/diff_output_test.rs` (create)

- [ ] **Step 1: Write the failing test**

```rust
//! Verify text / toon output formats for `cgn diff`.

use std::process::Command;
fn cgn_bin() -> &'static str { env!("CARGO_BIN_EXE_cgn") }

fn head_sha() -> String {
    let out = Command::new("git").args(["rev-parse", "HEAD"]).output().unwrap().stdout;
    String::from_utf8_lossy(&out).trim().to_string()
}

#[test]
fn diff_text_output_has_section_header() {
    let sha = head_sha();
    let output = Command::new(cgn_bin())
        .args(["diff", "--section", "bindings", "--baseline", &sha, "--format", "text"])
        .output()
        .expect("run");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Bindings") || stdout.contains("bindings"),
        "text output must label bindings section: {stdout}");
}

#[test]
fn diff_toon_output_parses() {
    let sha = head_sha();
    let output = Command::new(cgn_bin())
        .args(["diff", "--section", "bindings", "--baseline", &sha, "--format", "toon"])
        .output()
        .expect("run");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Toon's key:value structure: at minimum has a "baseline" or "sections" tag.
    assert!(
        stdout.contains("baseline") || stdout.contains("sections"),
        "toon output should mention baseline/sections: {stdout}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p code-graph-nexus --test diff_output_test`
Expected: `diff_toon_output_parses` may fail if toon path returns plain text fallback.

- [ ] **Step 3: Implement `output.rs`**

Create `crates/cgn-cli/src/commands/diff/output.rs`:

```rust
//! Format diff result envelope as text / json / toon.

use crate::commands::diff::bindings::BindingsDiff;
use crate::commands::diff::contracts::ContractsDiff;
use crate::commands::diff::routes::RoutesDiff;
use cgn_core::CgnError;
use serde_json::Value;

pub struct DiffEnvelope<'a> {
    pub baseline_ref: &'a str,
    pub baseline_sha: &'a str,
    pub current_ref: &'a str,
    pub current_sha: &'a str,
    pub bindings: Option<&'a BindingsDiff>,
    pub routes: Option<&'a RoutesDiff>,
    pub contracts: Option<&'a ContractsDiff>,
    pub verbose: bool,
}

pub fn emit(envelope: &DiffEnvelope, format: &str) -> Result<(), CgnError> {
    let json_value = build_json(envelope);
    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&json_value)
                .map_err(|e| CgnError::Output(format!("json: {e}")))?);
        }
        "toon" => {
            println!("{}", etoon::to_string(&json_value)
                .map_err(|e| CgnError::Output(format!("toon: {e}")))?);
        }
        _ => emit_text(envelope),
    }
    Ok(())
}

fn build_json(env: &DiffEnvelope) -> Value {
    let mut sections = serde_json::Map::new();
    if let Some(b) = env.bindings {
        sections.insert("bindings".into(), serde_json::to_value(b).unwrap_or(Value::Null));
    }
    if let Some(r) = env.routes {
        sections.insert("routes".into(), serde_json::to_value(r).unwrap_or(Value::Null));
    }
    if let Some(c) = env.contracts {
        sections.insert("contracts".into(), serde_json::to_value(c).unwrap_or(Value::Null));
    }
    serde_json::json!({
        "baseline": {"ref": env.baseline_ref, "sha": env.baseline_sha},
        "current": {"ref": env.current_ref, "sha": env.current_sha},
        "sections": sections,
    })
}

fn emit_text(env: &DiffEnvelope) {
    println!(
        "═══ Graph Δ ({} {} → {} {}) ═══",
        env.baseline_ref, &env.baseline_sha[..env.baseline_sha.len().min(7)],
        env.current_ref, &env.current_sha[..env.current_sha.len().min(7)],
    );
    if let Some(b) = env.bindings {
        println!("\n─ Section: bindings ─");
        println!("  new_resolutions: {}", b.new_resolutions.len());
        println!("  tier_changes:    {}", b.tier_changes.len());
        println!("  target_changes:  {}", b.target_changes.len());
        println!("  removed:         {}", b.removed.len());
        let limit = if env.verbose { usize::MAX } else { 10 };
        for chg in b.new_resolutions.iter().take(limit) {
            println!("  [NEW]    {}::{}", chg.src_file, chg.name);
        }
        for chg in b.tier_changes.iter().take(limit) {
            let from = chg.before.as_ref().and_then(|d| d.tier.as_deref()).unwrap_or("?");
            let to   = chg.after.as_ref().and_then(|d| d.tier.as_deref()).unwrap_or("?");
            println!("  [TIER]   {}::{} ({} → {})", chg.src_file, chg.name, from, to);
        }
    }
    if let Some(r) = env.routes {
        println!("\n─ Section: routes ─");
        println!("  added:    {}", r.added.len());
        println!("  removed:  {}", r.removed.len());
        println!("  modified: {}", r.modified.len());
    }
    if let Some(c) = env.contracts {
        println!("\n─ Section: contracts ─");
        println!("  added:    {}", c.added.len());
        println!("  removed:  {}", c.removed.len());
        println!("  modified: {}", c.modified.len());
    }
}
```

(Adapt: if `etoon` crate isn't a dep of cgn-cli yet, check `Cargo.toml`. If not present, add via `cargo add etoon` or fall back to a custom toon emitter that handles JSON-shaped data.)

- [ ] **Step 4: Replace inline emit in `mod.rs`**

In `crates/cgn-cli/src/commands/diff/mod.rs`, replace the inline `if args.format == "json" ...` block with:

```rust
let head_sha = baseline::resolve("HEAD", &repo_dir).unwrap_or_else(|_| "?".into());
output::emit(&output::DiffEnvelope {
    baseline_ref: &args.baseline,
    baseline_sha: &baseline_sha,
    current_ref: "HEAD",
    current_sha: &head_sha,
    bindings: bindings_diff.as_ref(),
    routes: routes_diff.as_ref(),
    contracts: contracts_diff.as_ref(),
    verbose: args.verbose,
}, &args.format)?;
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p code-graph-nexus --test diff_output_test`
Run: `cargo test -p code-graph-nexus --test diff_bindings_test`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/commands/diff/output.rs \
        crates/cgn-cli/src/commands/diff/mod.rs \
        crates/cgn-cli/tests/diff_output_test.rs
git commit -m "feat(diff): text/json/toon output formatters with truncation + --verbose"
```

---

### Task 13: `--section all` composition

**Files:**
- Modify: `crates/cgn-cli/src/commands/diff/mod.rs` (Already covered the `All` expansion via `want_*` flags; this task adds a test to lock it.)
- Test: `crates/cgn-cli/tests/diff_section_all_test.rs`

- [ ] **Step 1: Write the failing test**

```rust
//! `--section all` must produce the same JSON envelope as
//! `--section bindings,routes,contracts`.

use std::process::Command;
fn cgn_bin() -> &'static str { env!("CARGO_BIN_EXE_cgn") }

fn head_sha() -> String {
    let out = Command::new("git").args(["rev-parse", "HEAD"]).output().unwrap().stdout;
    String::from_utf8_lossy(&out).trim().to_string()
}

fn run_diff_json(sections: &str) -> serde_json::Value {
    let sha = head_sha();
    let out = Command::new(cgn_bin())
        .args(["diff", "--section", sections, "--baseline", &sha, "--format", "json"])
        .output()
        .expect("run");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn section_all_equals_explicit_three() {
    let a = run_diff_json("all");
    let b = run_diff_json("bindings,routes,contracts");
    let a_sections = a["sections"].as_object().unwrap();
    let b_sections = b["sections"].as_object().unwrap();
    for key in ["bindings", "routes", "contracts"] {
        assert_eq!(a_sections.get(key), b_sections.get(key),
            "section {key} must be identical between --section all and explicit list");
    }
}
```

- [ ] **Step 2: Run test to verify it fails or passes**

Run: `cargo test -p code-graph-nexus --test diff_section_all_test`
Expected: should already pass since `want_bindings/routes/contracts` flags handle `DiffSection::All`. If not, fix the matching logic in `mod.rs`.

- [ ] **Step 3: Confirm or fix `mod.rs` logic**

Ensure `mod.rs` contains:

```rust
let want_bindings = args.section.iter().any(|s|
    matches!(s, DiffSection::Bindings | DiffSection::All));
let want_routes = args.section.iter().any(|s|
    matches!(s, DiffSection::Routes | DiffSection::All));
let want_contracts = args.section.iter().any(|s|
    matches!(s, DiffSection::Contracts | DiffSection::All));
```

- [ ] **Step 4: Run test to confirm pass**

Run: `cargo test -p code-graph-nexus --test diff_section_all_test`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cgn-cli/tests/diff_section_all_test.rs \
        crates/cgn-cli/src/commands/diff/mod.rs
git commit -m "test(diff): pin --section all ≡ explicit sections list"
```

---

### Task 14: Skill doc update

**Files:**
- Modify: `~/.claude/skills/cgn/SKILL.md`

- [ ] **Step 1: Locate the Tool selection table**

Open `~/.claude/skills/cgn/SKILL.md`. Find the section labeled "Tool selection" with the table whose first row is `| Goal | Command |`.

- [ ] **Step 2: Add two new rows + amend admin row**

Insert these rows under the existing rows in the Tool selection table:

```markdown
| HTTP consumer → Route shape drift detection | `cgn shape_check --route <path>? --repo .` (no `--route` = all routes) |
| Cross-commit graph diff (bindings / routes / contracts) | `cgn diff --section <bindings\|routes\|contracts\|all> --baseline <ref> --repo .` (ref: branch/tag/SHA/HEAD~N/`PR/<n>`) |
```

Also update the admin row to mention the moved commands:

```markdown
| MCP host integration / install hooks / config TUI / **MCP server (`mcp serve\|tools`)** / **resolver vs LSP oracle benchmark (`verify-resolver`)** | `cgn admin` (hidden namespace) |
```

- [ ] **Step 3: Verify markdown renders cleanly**

Eyeball the file: table columns align, no broken pipes, no stray backticks.

- [ ] **Step 4: Commit skill doc**

```bash
git add ~/.claude/skills/cgn/SKILL.md
# Note: skill is in user dotfiles, not in this repo. If `~/.claude/skills/`
# is under git, commit there. Otherwise, leave the update local — repo
# changes don't track user skills.
```

If skill is in a separate repo:

```bash
cd ~/.claude
git add skills/cgn/SKILL.md
git commit -m "docs(cgn skill): add shape_check + diff; admin contains mcp/verify-resolver"
cd -
```

---

### Task 15: CLI surface snapshot + final integration

**Files:**
- Modify: `crates/cgn-cli/tests/cli_help_surface_test.rs` (extend with admin help snapshot)

- [ ] **Step 1: Append snapshot test**

```rust
#[test]
fn admin_help_contains_mcp_and_verify_resolver() {
    let output = Command::new(env!("CARGO_BIN_EXE_cgn"))
        .args(["admin", "--help"])
        .output()
        .expect("admin help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mcp"), "admin --help missing mcp: {stdout}");
    assert!(stdout.contains("verify-resolver"), "admin --help missing verify-resolver: {stdout}");
}

#[test]
fn top_level_help_contains_diff() {
    let output = Command::new(env!("CARGO_BIN_EXE_cgn"))
        .args(["--help"])
        .output()
        .expect("top help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("diff"), "top --help missing diff: {stdout}");
}
```

- [ ] **Step 2: Run full test suite**

Run: `cargo test --workspace 2>&1 | grep -E "test result|FAILED" | tail -20`
Expected: all previously-passing tests still pass, plus new diff / admin / surface tests.

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/cgn-cli/tests/cli_help_surface_test.rs
git commit -m "test(cli): pin admin/top-level help snapshot for tier-b commands"
```

- [ ] **Step 5: Open PR**

```bash
git push -u origin feat/tier-b-surface-and-diff
gh pr create --title "feat: Tier B CLI surface + cgn diff command" --body "$(cat <<'EOF'
## Summary

Implements Tier B spec (docs/superpowers/specs/2026-05-16-tier-b-surface-and-diff-design.md):

1. **CLI surface refactor**:
   - `mcp` (serve|tools) moved from top-level to `cgn admin mcp`
   - `verify-resolver` moved from top-level to `cgn admin verify-resolver`
   - `shape_check` un-hidden at top-level + `--route <path>` filter arg
   - `admin mcp tools` gains `--format <json|toon|text>`

2. **New `cgn diff` command** — generalized graph-level cross-commit diff:
   - `--section <bindings|routes|contracts|all>` (multi-select)
   - `--baseline <ref>` (branch / tag / SHA / HEAD~N / `PR/<n>`, no default)
   - `--format <text|json|toon>` + `--verbose`
   - Internally: stashes + checks out baseline SHA, runs analyzer, compares
   - GitGuard RAII restores branch/stash on drop

3. **Updated skill doc** to surface the new commands.

## Test plan

- [x] `cargo test --workspace` all green
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [x] Manual: `cgn admin mcp tools --format json` works
- [x] Manual: `cgn shape_check --route /api/users` filters
- [x] Manual: `cgn diff --section all --baseline origin/main` runs end-to-end

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Self-review notes

**Spec coverage:**
- §2.1 In scope items all map: Task 1 (mcp move), Task 2 (verify_resolver move), Task 3 (shape_check unhide), Task 4 (shape_check --route), Task 5 (mcp --format), Tasks 6-13 (cgn diff), Task 14 (skill doc), Task 15 (CLI surface tests).
- §2.2 Out of scope: symbols / edges sections, --oracle baseline mode, agent narrative output — all intentionally absent from the plan.
- §5.2 baseline ref forms covered in Task 7 baseline.rs.
- §5.6 exit code semantics: text emit always returns Ok in Task 9-12; errors only on dispatch/baseline/git failures.
- §7 Error handling: covered across Tasks 4 (no-match), 7 (invalid ref), 8 (panic recovery via Drop).

**Placeholder scan:**
- `(Adapt: ...)` annotations in Tasks 4, 5, 10, 11 mark places where the engineer must read existing code (RouteShape extraction, etoon crate availability, ZeroCopyGraph API). These are intentional reads, not handwave — file paths and what to look for are explicit.

**Type consistency:**
- `DiffSection` enum used in Task 6 referenced in Tasks 9-13 consistently.
- `BindingsDiff`, `RoutesDiff`, `ContractsDiff` types defined Tasks 9-11, consumed in Task 12 output.
- `GitGuard::enter` signature consistent across Tasks 8-11.

---

## Execution Handoff

Plan complete. Two execution options:

1. **Subagent-Driven (recommended)** — dispatch fresh subagent per task, review between tasks, fast iteration. Use `superpowers:subagent-driven-development`.

2. **Inline Execution** — execute tasks in this session using `superpowers:executing-plans`, batch execution with checkpoints.

Which approach?
