# Host Integration — register cgn as a "native" tool in LLM coding hosts

**Date**: 2026-05-15
**Status**: Draft (spec only — no implementation yet)
**Goal**: Document the three install paths by which `cgn` becomes a first-class tool in major LLM coding CLIs. All paths will be driven exclusively through `cgn admin` TUI; no top-level `cgn install` / `cgn integrate` command is to be exposed.

## Why this exists

LLM coding CLIs differ wildly in how external tools become callable by the
model:

- **Claude Code** (closed source, Anthropic) — only via MCP server
- **Gemini CLI** (Apache 2.0, TypeScript, Google) — MCP server *or* fork +
  add `BaseTool` subclass to `packages/core/src/tools/`
- **Codex CLI** (Apache 2.0, Rust, OpenAI) — MCP server *or* fork +
  in-process integration via workspace dependency on `cgn-cli`

Users want the "feels-like-grep" UX — model autonomously picks `cgn`
without prompt-engineering hints. From the model's perspective, MCP
tools and native tools are indistinguishable (both appear in the tool
list, both are picked autonomously). The only real differences are
operator-visible: process boundary, startup latency, dependency on a
running side-car.

The user has chosen all integration entry points live behind
`cgn admin` TUI — no top-level command exposure. Rationale: keeps the
public surface area minimal, treats host integration as an
out-of-band administrative concern rather than a per-project workflow
command.

## UX constraint (hard)

- **All install / uninstall flows MUST be reached via `cgn admin` TUI menu items.**
- **DO NOT add `cgn install`, `cgn integrate`, `cgn host-install`, or
  any sibling top-level subcommand.**
- The TUI's primary choice is *integration mechanism*, not host —
  user picks "how invasive" first (native fork vs MCP side-car), then
  the target host. Native is offered only where it's technically
  available; MCP is the universal fallback.
- The TUI menu structure (target):
  ```
  cgn admin
   ├── Bind tool to code agent
   │   ├── Native (no side-car; integrates into host's own tool registry)
   │   │   ├── Codex CLI (Rust workspace dep — zero IPC, mmap'd graph shared)
   │   │   └── Gemini CLI (TypeScript BaseTool — spawns cgn subprocess)
   │   └── MCP (one shared side-car serves any MCP-capable host)
   │       ├── Claude Code         ← only route Anthropic exposes
   │       ├── Cursor              ← supports MCP
   │       ├── Windsurf            ← supports MCP
   │       ├── Cline / Roo Code    ← supports MCP
   │       ├── Codex CLI           ← MCP route, no fork needed
   │       ├── Gemini CLI          ← MCP route, no fork needed
   │       └── (any other MCP-capable host) — generic registration writer
   ├── (future) Diagnostics
   └── (future) Index maintenance
  ```
- **Codex / Gemini show up in both branches** — Native is the
  zero-IPC path that requires a fork; MCP is the no-fork side-car
  path. Same `cgn` binary serves both. Pick whichever trade-off you
  prefer per host.
- Each leaf provides three actions: install / uninstall / status.
- Status detection: probes whether the install is present (file exists
  / config entry present / workspace dep declared) and reports
  `installed` / `missing` / `outdated`.
- **Native and MCP are not mutually exclusive at the host level** — a
  user could install cgn as native into Codex AND also install the
  shared MCP side-car for Claude Code. The TUI tracks each leaf
  independently. The branches in the menu correspond to install
  *mechanisms*, not user "modes".

> **Reading order**: paths below are now grouped by *mechanism* (Native
> first, MCP second) to match the TUI hierarchy. Within each mechanism,
> hosts are listed in the order they appear in the menu.

## Mechanism: MCP (shared side-car for any MCP-capable host)

### Path 1 — Claude Code (MCP server)

**Why MCP for Claude Code**: Claude Code is closed source. There is no
public extension point that injects a tool into the model's
first-class tool list other than MCP. Skills / hooks / CLAUDE.md are
prompt-layer mechanisms; the model sees them as text, not tools.

### Pre-reqs

- Claude Code installed (`claude` on PATH)
- `cgn` binary on PATH (the existing CLI; MCP mode is a subcommand of
  the same binary, not a separate executable — see "Single-binary
  model" below)

### Single-binary model: `cgn mcp serve` with dual dispatch modes

A new crate `crates/cgn-mcp/` exposes the **server library**
(stdio JSON-RPC handler, tool dispatch, schema generation). The
existing `cgn-cli` crate adds a thin `mcp` subcommand that
wraps the library:

```bash
# CLI mode — existing usage unchanged
cgn context --name foo

# MCP server mode (Fresh, default) — each tool call spawns a fresh CLI
cgn mcp serve

# MCP server mode (Fast, opt-in) — Engine stays mmap'd for server lifetime
cgn mcp serve --daemon

# Debug — list tools exposed via inventory + their schemas
cgn mcp tools
```

The user picks the mode at TUI install time. Two dispatch back-ends
share the same inventory-based tool registry; only the per-call
dispatch differs.

### Why dual mode

LLM coding hosts have two distinct usage profiles:

- **Interactive** (Claude Code / Cursor / etc. typing-with-user) —
  tool calls are dwarfed by LLM thinking time (2-30s/turn). A
  ~15-100ms latency difference per call is invisible. Memory
  footprint and graph freshness matter more than µs latency.
- **Batch / scripted** — many tool calls back-to-back, latency floor
  matters. Per-call spawn cost compounds.

Spawn mode wins on freshness + isolation + simplicity; daemon wins
on raw latency. Letting the operator pick at install time avoids
forcing one trade-off on everyone.

### Latency comparison (predicted)

Verified against README's measured CLI end-to-end latencies:

| Tool | CLI alone | Daemon mode | Spawn mode (Linux) |
|---|---|---|---|
| `context` | 9 ms | ~12 ms | ~24 ms |
| `impact` | 5-6 ms | ~8 ms | ~20 ms |
| `route_map` | 13 ms | ~16 ms | ~28 ms |
| `query` (BM25) | 24 ms | ~27 ms | ~39 ms |
| `detect_changes` | 230 ms | ~233 ms | ~245 ms |

Spawn-mode overhead is 10-15 ms on Linux per call, 15-30 ms on macOS,
30-80 ms on Windows (process creation costs). All are negligible
beside LLM thinking time.

### Daemon-mode stale-graph mitigation (mtime-remap)

POSIX file replacement (write-tmp + atomic rename, which `cgn
analyze` uses — see `crates/cgn-core/src/registry/io.rs:17-35`)
swaps the dentry but the daemon's existing mmap still points at the
unlinked old inode. Without mitigation, daemon serves stale data
after every `cgn analyze` until restart.

Solution (~30 LOC): stat the graph file's mtime before each
dispatch; remap if changed:

```rust
// cgn-mcp/src/daemon.rs
fn ensure_fresh(engine: &mut Engine, path: &Path) -> Result<()> {
    let mtime = fs::metadata(path)?.modified()?;
    if mtime > engine.loaded_at {
        *engine = Engine::load(path)?;       // new fd → new inode
        // old Engine drops → old mmap released → old inode finally freed
    }
    Ok(())
}
```

Stat is <0.1 ms (one syscall); cost is dwarfed by mmap-load when
remap is actually needed (~1-5 ms). Daemon mode is therefore
"fresh by next call after analyze".

Host config writes one of:
```json
// Fresh (default)
{"mcpServers":{"cgn":{"command":"cgn","args":["mcp","serve"]}}}

// Fast (opt-in)
{"mcpServers":{"cgn":{"command":"cgn","args":["mcp","serve","--daemon"]}}}
```

One binary, two modes, no separate `cgn-mcp` install step.

### Shared business-logic refactor (precondition)

Both CLI and MCP code paths must call the same business logic
without re-implementation. The refactor splits every
`commands/<x>.rs` into two halves:

```rust
// commands/context.rs

// 1. Args struct gains 3 extra derives — no behavioural change for CLI.
#[derive(Args, Serialize, Deserialize, JsonSchema)]
pub struct ContextArgs { /* fields */ }

// 2. Business logic moves into run_inner — returns structured JSON.
pub fn run_inner(args: ContextArgs, engine: &Engine)
    -> Result<serde_json::Value>;

// 3. CLI entry becomes a thin wrapper: run = run_inner + emit().
pub fn run(args: ContextArgs, engine: &Engine) -> Result<()> {
    let value = run_inner(args, engine)?;
    emit(&value, args.format)
}
```

MCP side consumes `run_inner` directly, **renders output as TOON
(token-efficient), not raw JSON** — preserves the project's
token-cheapest-output principle. **Zero hardcoding** of which tools
exist: each command self-registers via the `inventory` crate at link
time; the MCP crate is tool-agnostic.

### MCP crate infrastructure — registry shared by both modes (~100 LOC)

```rust
// cgn-mcp/src/registry.rs

/// Single registration captures everything both modes need.
pub struct CgnMcpTool {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: fn() -> schemars::schema::RootSchema,

    /// Daemon-mode dispatch — call `run_inner` in-process.
    pub handler: fn(serde_json::Value, &Engine)
        -> Result<serde_json::Value, CgnError>,

    /// Spawn-mode dispatch — what subcommand to spawn.
    /// Auto-derived from `module_path!()` — e.g. "context".
    pub subcommand: &'static str,
}
inventory::collect!(CgnMcpTool);

pub fn register_all(server: &mut McpServer, mode: DispatchMode) {
    for tool in inventory::iter::<CgnMcpTool>() {
        server.register(tool.name, tool.description, (tool.schema)(),
            match mode {
                DispatchMode::Daemon(ref engine_cell) => {
                    daemon_handler(tool, engine_cell.clone())
                }
                DispatchMode::Spawn => {
                    spawn_handler(tool)
                }
            });
    }
}
```

### Daemon-mode handler (~50 LOC)

```rust
// cgn-mcp/src/daemon.rs
fn daemon_handler(tool: &'static CgnMcpTool, engine_cell: Arc<Mutex<Engine>>)
    -> impl Fn(Value) -> Result<ToolResult>
{
    move |args| {
        let mut engine = engine_cell.lock().unwrap();
        ensure_fresh(&mut engine, &engine.path)?;     // mtime-remap
        let value = (tool.handler)(args, &engine)?;
        let body = output::emit_to_string(&value, OutputFormat::Toon)?;
        Ok(ToolResult::text(body))
    }
}
```

### Spawn-mode handler (~80 LOC)

```rust
// cgn-mcp/src/spawn.rs
fn spawn_handler(tool: &'static CgnMcpTool)
    -> impl Fn(Value) -> Result<ToolResult>
{
    move |args| {
        let argv = json_to_argv(&args)?;
        let self_exe = std::env::current_exe()?;
        let output = std::process::Command::new(self_exe)
            .arg(tool.subcommand)
            .args(argv)
            .output()?;
        if output.status.success() {
            // CLI already emitted TOON / per-command-default to stdout.
            Ok(ToolResult::text(String::from_utf8_lossy(&output.stdout).into_owned()))
        } else {
            Ok(ToolResult::error(String::from_utf8_lossy(&output.stderr).into_owned()))
        }
    }
}
```

### `json_to_argv` — the only non-trivial new conversion (~30 LOC)

Translates MCP's JSON args into the clap CLI flag form cgn
subcommands expect. JsonSchema-driven for type fidelity (bool flags
vs string opts vs numeric).

```rust
// {"name": "foo", "uid": null}  →  ["--name", "foo"]
// {"includeTests": true}        →  ["--include-tests"]
// {"includeTests": false}       →  []
```

Vec<T> and nested objects are out of MVP scope; cgn's existing args
are all flat (no command takes a nested object).

### Per-command self-registration (one line at the bottom of each `commands/<x>.rs`)

```rust
// commands/context.rs

/// Look up a symbol's definition, callers, callees, surrounding context.
#[derive(Args, Serialize, Deserialize, JsonSchema)]
pub struct ContextArgs { /* fields with /// doc comments */ }

pub fn run_inner(args: ContextArgs, engine: &Engine)
    -> Result<serde_json::Value, CgnError> { /* business logic */ }

pub fn run(args: ContextArgs, engine: &Engine) -> Result<(), CgnError> {
    let value = run_inner(args, engine)?;
    emit(&value, args.format)
}

// ← single line; tool name auto-derived from module_path!()
//   ("...::commands::context" → "cgn_context")
cgn_register_mcp_tool!(ContextArgs, run_inner);
```

### The registration macro (one definition, in `cgn-mcp::macros`)

The macro fills both mode fields from one declaration. Daemon mode
uses `handler`; spawn mode uses `subcommand`. Both auto-derived.

```rust
#[macro_export]
macro_rules! cgn_register_mcp_tool {
    ($args:ty, $inner:path) => {
        inventory::submit! {
            $crate::registry::CgnMcpTool {
                name: $crate::registry::derive_tool_name(module_path!()),
                description: <$args as ::schemars::JsonSchema>::schema_name(),
                schema: || ::schemars::schema_for!($args),
                // Daemon mode: in-process handler.
                handler: |raw, engine| {
                    let parsed: $args = ::serde_json::from_value(raw)?;
                    $inner(parsed, engine)
                },
                // Spawn mode: subcommand auto-derived from module path.
                subcommand: $crate::registry::derive_subcommand(module_path!()),
            }
        }
    };
}
```

### What "zero hardcode" means concretely

- **MCP crate** has NO list of commands. It iterates `inventory::iter::<CgnMcpTool>()`.
- **Adding a 9th command** requires only: write `commands/foo.rs` with `Args + run_inner + cgn_register_mcp_tool!(FooArgs, run_inner)`. Zero changes to MCP crate.
- **Tool name** is derived from `module_path!()`; no string literal anywhere.
- **Tool description** is derived from the `/// doc comment` on the Args struct via schemars; no separate description text.
- **Tool input schema** is derived from the `JsonSchema` derive; no hand-written schema JSON.

### Output handling refactor (unchanged)

- `output::emit_to_string(value, format) -> Result<String>` — new helper,
  same serialization logic the CLI uses, returns String instead of
  writing to stdout.
- `output::emit(value, format)` becomes `println!("{}", emit_to_string(value, format)?)`.

The CLI's per-command default format (some commands default `toon`,
some `text`, some `compact` — already token-optimized per the
README) is preserved on the CLI path. MCP defaults to TOON across all
tools for consistency and best token density.

### Constraint disclosure

Rust has no Python-style runtime module reflection (no
`importlib.import_module` / `dir(commands)`). The `inventory` crate
uses linker-section tricks to collect static submissions at program
start — items only land in the iter if their containing module is
actually compiled into the binary. Since every `commands/<x>.rs`
already has `pub mod xxx;` in `commands/mod.rs` (else the CLI
subcommand wouldn't work), the `inventory` collection is complete
by construction. **No platform caveats for our target environments**
(Linux / macOS / Windows native binaries); WASM is not a target.

Output handling refactor (paired with `run_inner` split):

- `output::emit_to_string(value, format) -> Result<String>` — new helper,
  same serialization logic the CLI uses, but returns a String instead
  of writing to stdout.
- `output::emit(value, format)` becomes `println!("{}", emit_to_string(value, format)?)`.

The CLI's per-command default format (some commands default `toon`,
some `text`, some `compact` — already token-optimized per the
README) is preserved on the CLI path. MCP defaults to TOON across all
tools for consistency and best token density.

JSON schema for MCP tool definition is generated automatically from
the `JsonSchema` derive (via the `schemars` crate). **No hand-written
tool schemas.** Both sides read the same source of truth.

Refactor scope: 28 `commands/*.rs` files, +10-15 LOC each, total
~300-400 LOC mechanical refactor. **Must land before
`cgn-mcp` crate work begins.**

### Tool set (initial — auto-discovered, this list is informational)

The MCP crate doesn't enumerate tools; it iterates whatever
`inventory` collects. The list below is the **expected** set after
all `commands/*.rs` ship their one-line registration. Each name is
auto-derived from `module_path!()`:

| Auto-derived MCP tool name | Source module |
|---|---|
| `cgn_context` | `commands::context` |
| `cgn_impact` | `commands::impact` |
| `cgn_query` | `commands::query` |
| `cgn_detect_changes` | `commands::detect_changes` |
| `cgn_rename` | `commands::rename` |
| `cgn_route_map` | `commands::route_map` |
| `cgn_shape_check` | `commands::shape_check` |
| `cgn_multi_query` | `commands::multi_query` |

Test invariant: `cgn mcp tools` must list these 8 names exactly. If
a command file forgets `cgn_register_mcp_tool!`, the test fails
loudly. New commands appear in `cgn mcp tools` the moment the macro
line is added — no MCP-crate change required.

Rust MCP SDK: `rmcp` (Anthropic-blessed) primary candidate; `mcp-rs`
as fallback. Final pick deferred to implementation phase.

### Registration steps (idempotent upsert)

The TUI never silently overwrites. Three-state install logic, applied
uniformly to every MCP host:

1. Probe `claude --version` (or equivalent for each host) — abort with
   friendly message if host not installed.
2. Atomic read-modify-write on the host's config file with **upsert**
   semantics. Read the existing `mcpServers.cgn` entry (if any) and
   compare against the entry we'd write:
   - **State A — no existing entry** → write it, report `installed`.
   - **State B — entry matches what we'd write** (same `command`,
     `args`, `env`) → no-op, report `already up to date`. Exit cleanly.
   - **State C — entry exists but differs** (e.g. user previously
     installed in spawn mode and is now picking daemon, or `cgn`
     binary moved on PATH) → display a unified diff:
     ```
     Existing cgn entry:
       command: cgn
       args:    ["mcp", "serve"]
     New entry:
       command: cgn
       args:    ["mcp", "serve", "--daemon"]
     Update? [y/N]
     ```
     On confirm → overwrite. On decline → abort, leave existing entry
     untouched, exit non-zero so scripts can detect.
3. Print uninstall reminder: `cgn admin → Bind tool to code agent → MCP → Claude Code → uninstall`.

Atomicity contract: the file write goes through `atomic_write_json` —
tmp + fsync + rename — so partial writes never corrupt the host's
config. Other `mcpServers` entries (non-`cgn` keys) are read in,
preserved through the modify cycle, and written back untouched.

### Uninstall

Remove the `"cgn"` key from the same JSON file; leave other entries
untouched (read-modify-write, atomic). If no `cgn` key exists, report
`not installed` and exit cleanly (idempotent — uninstalling something
that isn't there is a no-op, not an error).

### Status probe

`cgn admin` reads the JSON file and reports one of:
- `installed (mode=spawn)` / `installed (mode=daemon)` — entry exists
  and matches current binary location;
- `outdated` — entry exists but `command` path doesn't match
  `which cgn`, OR `args` indicates a mode we no longer support;
- `missing` — no `cgn` entry.

Status output never modifies the file. Useful in scripts to
conditionally run install.

### Path 1b — Cursor / Windsurf / Cline / Roo Code / generic MCP host

All four of these (and any future MCP-capable host) consume the **same
`cgn` binary** as Claude Code, invoked the same way (`cgn mcp serve`
or `cgn mcp serve --daemon`). The only difference is where the
registration entry lives:

| Host | Registration file | Format |
|---|---|---|
| Claude Code | `~/.config/claude-code/mcp-servers.json` or `.mcp.json` (project) | JSON `mcpServers` object |
| Cursor | `~/.cursor/mcp.json` or `.cursor/mcp.json` (project) | JSON `mcpServers` object (same shape) |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` | JSON |
| Cline / Roo Code | VS Code settings: `cline.mcpServers` | JSON inside `settings.json` |
| Codex CLI (MCP variant) | `~/.codex/mcp.json` (or `.codex/mcp.json` per project) | JSON `mcpServers` object |
| Gemini CLI (MCP variant) | `~/.gemini/settings.json` → `mcpServers` field | JSON inside settings |
| Other / unknown | TUI prints the JSON snippet + instructs user to paste into their host's config | Manual |

**Codex and Gemini appear in both branches** — the Native menu offers
the fork-patch route for users who want zero-IPC integration; the MCP
menu offers the side-car route for users who don't want to maintain a
fork. Same cgn binary serves both. The two routes are not mutually
exclusive on a per-host basis, but the TUI's status probe will note
both as installed if a user does both (which is harmless — the model
will see the tools once via whichever path the host loaded first).

The TUI's MCP branch dispatches on host pick → writes to the right
file with the right schema variant. The side-car binary itself is
host-agnostic — install it once, register it N times.

## Mechanism: Native (no side-car; host's own tool registry)

### Path 2 — Gemini CLI (fork patch)

**Why fork**: Gemini CLI is open source. The "feels native" route is to
add a `BaseTool` subclass that joins the built-in tool registry. MCP
also works but operator-visible as "MCP tool" (vs Gemini's built-ins
like `ReadFile`).

### Pre-reqs

- A local clone of [`google-gemini/gemini-cli`](https://github.com/google-gemini/gemini-cli)
- The user's fork remote set up (so they can keep custom patches without
  fighting `git pull` against `main`)
- `cgn` binary on PATH (since the Gemini-side tool wrapper invokes
  `cgn` as a subprocess — there's no shared Rust↔TS in-process path)

### File layout in the fork

```
packages/core/src/tools/
└── cgn/
    ├── index.ts          ← exports & registry hooks
    ├── context-tool.ts   ← class CgnContextTool extends BaseTool
    ├── impact-tool.ts    ← class CgnImpactTool extends BaseTool
    ├── query-tool.ts     ← class CgnQueryTool extends BaseTool
    └── ...               ← one file per cgn subcommand
```

Each tool class:
1. Declares schema via the existing `BaseTool` schema builder.
2. `execute()` spawns `cgn <subcmd>` with parsed args and returns the
   stdout/stderr as a `ToolResult`.

### Registry wiring

Edit `packages/core/src/tools/tool-registry.ts` (or whichever file
contains `registerBuiltinTools()`):

```typescript
import { CgnContextTool, CgnImpactTool, /* ... */ } from './cgn/index.js';

function registerBuiltinTools(registry: ToolRegistry) {
  // ... existing built-ins ...
  registry.register(new CgnContextTool());
  registry.register(new CgnImpactTool());
  // ...
}
```

### Distribution model

The TUI does NOT auto-fork or auto-patch the user's Gemini install.
Instead it:

1. Probes if `gemini --version` resolves.
2. Writes a patch file to `~/.config/cgn/host-integration/gemini-cli.patch`
   (template ships inside the `code-graph-nexus` crate as an embedded
   resource).
3. Prints the manual steps:
   ```
   cd <your gemini-cli fork>
   git apply ~/.config/cgn/host-integration/gemini-cli.patch
   npm run build
   npm link  # so the patched gemini takes precedence
   ```
4. Status probe: looks for the embedded marker string
   `// cgn-integration-marker-v1` in the user-supplied
   `gemini-cli` checkout (path provided in step 1).

### Maintenance burden disclosure

The TUI's install screen MUST display:
> "Forking Gemini CLI means re-applying this patch every time
> upstream gemini-cli releases. Expect ~10 minutes per upstream
> release. If that's too much overhead, use MCP integration instead."

## Path 3 — Codex CLI (Rust in-process workspace dep)

**Why this is the sweet spot for cgn**: Codex CLI is Rust. cgn is
Rust. The Codex CLI workspace can take a path/git dependency on the
`cgn-cli` crate and call its `commands::*::run` functions
directly. No spawn, no IPC, no startup latency, mmap'd graph shared
across the same address space.

### Pre-reqs

- A local clone of [`openai/codex`](https://github.com/openai/codex)
- The user's fork remote
- This `code-graph-nexus-rs` repo cloned locally (or published to crates.io
  in a future iteration — current state: not yet on crates.io due to
  vendored grammars)

### Codex CLI fork modifications

`codex-rs/core/Cargo.toml`:
```toml
[dependencies]
cgn-cli = { path = "../../../code-graph-nexus-rs/crates/cgn-cli" }
cgn-core = { path = "../../../code-graph-nexus-rs/crates/cgn-core" }
```

`codex-rs/core/src/tools/cgn.rs` (new):
```rust
use cgn_cli::commands::context;
use cgn_cli::engine::Engine;

pub struct CgnContext;

impl Tool for CgnContext {
    fn name(&self) -> &str { "cgn_context" }

    fn schema(&self) -> ToolSchema { /* JSON schema for ContextArgs */ }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: context::ContextArgs = serde_json::from_value(args)?;
        let engine = Engine::load(/* repo-resolved graph path */)?;
        // Capture stdout via a String sink rather than spawning.
        context::run(parsed, &engine)
            .map(|out| ToolResult::text(out))
    }
}
```

Registry wiring in `codex-rs/core/src/tools/mod.rs` analogous to the
Gemini path.

### Distribution model — same as Gemini

The TUI ships a patch file at
`~/.config/cgn/host-integration/codex-cli.patch` and prints manual
apply / build instructions. Auto-patching the user's fork is rejected
(too easy to corrupt their working tree).

### Capture-stdout concern

The current `commands::*::run` functions all write to stdout via the
`output::emit` helper. For in-process integration we need either:
1. A thread-local stdout redirector that captures emit() output, OR
2. A new `run_to_string` variant of each command that returns a
   `String` instead of writing to stdout.

Option (2) is cleaner but touches every command. Option (1) is a
one-line wrapper in the Codex-side tool. **Defer the decision** — the
spec just notes the constraint.

## TUI implementation rough sketch (out of scope for this spec)

```
crates/cgn-cli/src/commands/admin/
├── mod.rs              ← clap entrypoint + TUI bootstrap
├── menu.rs             ← top-level menu (ratatui or dialoguer)
├── host_integration/
│   ├── mod.rs
│   ├── claude_code.rs  ← MCP JSON read/write
│   ├── gemini_cli.rs   ← patch write + manual-step printer
│   └── codex_cli.rs    ← patch write + manual-step printer
└── ... (other admin areas, future)
```

`cgn admin` would be the only new top-level command added by this
spec. Its subcommands stay inside the TUI — never exposed as
`cgn admin install-claude-code` etc.

## Open questions (do not implement until resolved)

1. **Patch versioning**: how to keep the embedded Gemini/Codex patches
   in sync with upstream Gemini/Codex releases? Options: pin to a
   specific upstream commit in the patch header and warn if user's
   fork is ahead; generate the patch on demand from a template + the
   current cgn tool list.
2. **Tool naming collision**: if upstream gemini-cli or codex-cli adds
   a `cgn`-prefixed tool someday, our patch breaks. Mitigation: use
   `cgn_<tool>` prefix instead of `cgn_<tool>`? The spec
   leaves this open.
3. **MCP server transport**: stdio is the simplest. Worth supporting
   HTTP+SSE later (for remote-graph use case)? Out of scope here.
4. **Capture-stdout vs `run_to_string`**: decide before Codex-CLI
   integration ships.
5. **Discoverability**: how does a user know `cgn admin` exists? README
   line + `cgn --help` mentions it. Both required.

## Non-goals (explicit)

- Auto-fork / auto-PR upstream gemini-cli / codex-cli on the user's behalf
- Cursor / Windsurf / ChatGPT integration (each requires a separate spec
  if it lands later — same TUI menu, separate handler)
- Self-hosted MCP transport (HTTP+SSE)
- VS Code / JetBrains plugin (different ecosystem entirely)

## Decision log

| Decision | Choice | Rationale |
|---|---|---|
| Top-level command name | `cgn admin` | Sibling to `cgn config`; "admin" signals "one-off setup" vs `config`'s "edit current settings". |
| Install command exposure | TUI-only, no flat `cgn install` | User constraint. Keeps `cgn` public surface minimal; install is admin work. |
| Menu top-level grouping | By **mechanism** (Native / MCP), not by host | User-clarified 2026-05-15: Codex and Gemini both support MCP *and* native; grouping by host would force a false either-or per host. Grouping by mechanism lets the user pick "how invasive" first and surfaces that MCP is the universal fallback for everything Anthropic-grade and below. |
| MCP side-car scope | One binary serves all MCP hosts | The `cgn` binary's `mcp serve` subcommand is host-agnostic — Claude Code, Cursor, Windsurf, Cline all consume the same stdio JSON-RPC interface. The TUI's only per-host work is writing to the right config file. |
| Binary layout | Single `cgn` binary; MCP via `cgn mcp serve` subcommand | User-clarified 2026-05-15: avoids second `cgn-mcp` install / PATH entry. Same binary, two execution modes. The `cgn-mcp` crate provides the server library; `cgn-cli` adds the `mcp` subcommand wrapper. |
| Args + business-logic sharing | Refactor `commands/<x>.rs::run` → `run_inner` (returns Value) + `run` (calls inner + emit) | User-clarified 2026-05-15: no schema or output duplication between CLI and MCP. Args struct adds `Serialize, Deserialize, JsonSchema` derives; schema auto-generated via `schemars::schema_for!`. MCP wrapper consumes `run_inner` directly. Precondition for cgn-mcp crate work. |
| MCP output format | TOON by default (`ToolResult::text(toon_body)`) | User-clarified 2026-05-15: raw JSON would defeat the token-economy thesis. Reuse CLI's `emit_to_string(value, OutputFormat::Toon)` helper so MCP and CLI share serialization. Model can override via per-call `format` arg if it specifically needs JSON. |
| Output helper refactor | Add `output::emit_to_string()` returning String | Split serialization from stdout-write so both CLI (`emit`) and MCP (`ToolResult::text`) consume the same code. Paired with the `run_inner` refactor. |
| MCP tool discovery | `inventory` crate + per-command self-registration via `cgn_register_mcp_tool!` macro | User-clarified 2026-05-15: zero hardcoded tool list in MCP crate. Adding a command = one macro line in that command's own file. MCP crate iterates `inventory::iter::<CgnMcpTool>()`. Tool name from `module_path!()`, description from `JsonSchema` doc comment — no string literals anywhere. |
| Self-registration completeness invariant | Test asserts `cgn mcp tools` reports exactly the 8 known commands | Catches "forgot to add the macro line" silent regressions. Adding a 9th command updates the test. |
| MCP dispatch architecture | Dual-mode: spawn (default) + daemon (opt-in via `--daemon`) | User-clarified 2026-05-15: spawn gives freshness + isolation + simplicity wins on the LLM interactive use case; daemon wins on raw latency for batch/scripted use. Picking either alone forces an unwanted trade-off on the other use case. Shared inventory means dual-mode costs only ~510 LOC vs ~490 spawn-only — minor surcharge for user flexibility. |
| Default mode | Spawn (Fresh) | Safer defaults — no stale graph, no memory footprint, no mtime-remap edge cases. Power users opt into daemon via the TUI mode picker. |
| Daemon stale-graph mitigation | mtime-remap before each dispatch (~30 LOC) | POSIX `rename()` (used by `cgn analyze` via `crates/cgn-core/src/registry/io.rs:17-35`) swaps dentry but mmap holds the unlinked old inode. Without mitigation, daemon serves stale data until restart. `fs::metadata().modified()` follows dentry → returns new inode's mtime → daemon detects and remaps. Cost: one syscall per call (<0.1ms). |
| TUI mode picker | Asked at MCP install time per host | TUI prompts "Fast (daemon) ~12ms/call, 50-500MB resident, auto-refresh on cgn analyze · Fresh (spawn) ~24ms/call, <10MB resident, always 100% fresh". Writes appropriate `args` array into host config. User can re-run install to switch modes later. |
| Install semantics | Three-state idempotent upsert | User-clarified 2026-05-15: re-running install when entry already exists must NOT silently overwrite. State A (no entry) → write. State B (entry matches) → no-op, report `already up to date`. State C (entry differs) → show diff + `y/N` confirm. Uninstall is idempotent — removing a non-existent entry is a no-op, not an error. Other `mcpServers` entries are preserved through every read-modify-write cycle. |
| Native install automation | Manual (TUI prints patch + steps) | Too easy to corrupt user's git tree. Auto-`git apply` rejected. |
| MCP install automation | Fully automated (atomic JSON write) | All MCP host config files are well-known JSON; safe to auto-write with read-modify-write atomicity. TUI just asks "which host?" then writes the entry. |
| Native scope | Codex + Gemini only | User-clarified 2026-05-15: every other host is either closed source (Cursor/Copilot/Windsurf/Claude Code) or open source with too-small user base to justify per-fork maintenance (Cline/Roo/Aider/Continue). MCP catches all of those. |
| MCP host coverage | Claude Code, Cursor, Windsurf, Cline/Roo, **Codex CLI, Gemini CLI**, Copilot Extensions, generic paste-it | User-clarified 2026-05-15 (correction): Codex and Gemini also appear under the MCP branch — both support MCP as a no-fork alternative. The two branches are not mutually exclusive per host. Each MCP leaf gets a TUI handler with the right config-file writer; the `cgn mcp serve` invocation is identical across hosts. |
| Auto-patch user forks | No | Too easy to corrupt user's git tree. TUI writes patch file + prints manual steps. |
| Claude Code integration route | MCP only | Closed source — no other route exists. |
| Codex CLI integration route | Workspace dep (in-process) | Same language; zero IPC overhead; mmap'd graph shared. |
| Gemini CLI integration route | Subprocess via patch | Cross-language (TS↔Rust) — spawn `cgn` is the cleanest boundary. |
