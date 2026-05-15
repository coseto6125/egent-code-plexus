# Host Integration — register gnx as a "native" tool in LLM coding hosts

**Date**: 2026-05-15
**Status**: Draft (spec only — no implementation yet)
**Goal**: Document the three install paths by which `gnx` becomes a first-class tool in major LLM coding CLIs. All paths will be driven exclusively through `gnx admin` TUI; no top-level `gnx install` / `gnx integrate` command is to be exposed.

## Why this exists

LLM coding CLIs differ wildly in how external tools become callable by the
model:

- **Claude Code** (closed source, Anthropic) — only via MCP server
- **Gemini CLI** (Apache 2.0, TypeScript, Google) — MCP server *or* fork +
  add `BaseTool` subclass to `packages/core/src/tools/`
- **Codex CLI** (Apache 2.0, Rust, OpenAI) — MCP server *or* fork +
  in-process integration via workspace dependency on `graph-nexus-cli`

Users want the "feels-like-grep" UX — model autonomously picks `gnx`
without prompt-engineering hints. From the model's perspective, MCP
tools and native tools are indistinguishable (both appear in the tool
list, both are picked autonomously). The only real differences are
operator-visible: process boundary, startup latency, dependency on a
running side-car.

The user has chosen all integration entry points live behind
`gnx admin` TUI — no top-level command exposure. Rationale: keeps the
public surface area minimal, treats host integration as an
out-of-band administrative concern rather than a per-project workflow
command.

## UX constraint (hard)

- **All install / uninstall flows MUST be reached via `gnx admin` TUI menu items.**
- **DO NOT add `gnx install`, `gnx integrate`, `gnx host-install`, or
  any sibling top-level subcommand.**
- The TUI's primary choice is *integration mechanism*, not host —
  user picks "how invasive" first (native fork vs MCP side-car), then
  the target host. Native is offered only where it's technically
  available; MCP is the universal fallback.
- The TUI menu structure (target):
  ```
  gnx admin
   ├── Bind tool to code agent
   │   ├── Native (no side-car; integrates into host's own tool registry)
   │   │   ├── Codex CLI (Rust workspace dep — zero IPC, mmap'd graph shared)
   │   │   └── Gemini CLI (TypeScript BaseTool — spawns gnx subprocess)
   │   └── MCP (one shared side-car serves any MCP-capable host)
   │       ├── Claude Code         ← only route Anthropic exposes
   │       ├── Cursor              ← supports MCP
   │       ├── Windsurf            ← supports MCP
   │       ├── Cline / Roo Code    ← supports MCP
   │       └── (any other MCP-capable host) — generic registration writer
   ├── (future) Diagnostics
   └── (future) Index maintenance
  ```
- Each leaf provides three actions: install / uninstall / status.
- Status detection: probes whether the install is present (file exists
  / config entry present / workspace dep declared) and reports
  `installed` / `missing` / `outdated`.
- **Native and MCP are not mutually exclusive at the host level** — a
  user could install gnx as native into Codex AND also install the
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
- `gnx` binary on PATH (the existing CLI; MCP mode is a subcommand of
  the same binary, not a separate executable — see "Single-binary
  model" below)

### Single-binary model: `gnx mcp serve`

A new crate `crates/graph-nexus-mcp/` exposes the **server library**
(stdio JSON-RPC handler, tool dispatch, schema generation). The
existing `graph-nexus-cli` crate adds a thin `mcp` subcommand that
wraps the library:

```bash
# CLI mode — existing usage unchanged
gnx context --name foo

# MCP server mode — what hosts invoke
gnx mcp serve            # stdio JSON-RPC server, blocks until host disconnects
gnx mcp tools            # list exposed tools (debug)
```

Host config writes `command: "gnx", args: ["mcp", "serve"]` —
one binary serves all callers, no separate `gnx-mcp` install step,
no PATH duplication.

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

MCP side then consumes `run_inner` directly, **but renders output as
TOON (token-efficient), not raw JSON** — this preserves the project's
token-cheapest-output principle which is the whole point of the Rust
port vs upstream:

```rust
// graph-nexus-mcp/src/tools.rs
async fn context_handler(args: Value, engine: &Engine) -> ToolResult {
    let parsed: ContextArgs = serde_json::from_value(args)?;
    let value = commands::context::run_inner(parsed, engine)?;

    // Default to TOON for max token economy.  Model may override per
    // call via an extra `format` arg if it specifically needs JSON
    // for downstream parsing (mirrors gnx CLI's --format flag).
    let format = parsed.format.unwrap_or(OutputFormat::Toon);
    let body = output::emit_to_string(&value, format)?;
    ToolResult::text(body)   // text content, NOT json content
}
```

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
`graph-nexus-mcp` crate work begins.**

### Tool set (initial)

Tools exposed via MCP (each name `gnx_<subcommand>` mirrors upstream
gitnexus for familiarity):

| MCP tool | Wraps `run_inner` of |
|---|---|
| `gnx_context` | `commands::context` |
| `gnx_impact` | `commands::impact` |
| `gnx_query` | `commands::query` |
| `gnx_detect_changes` | `commands::detect_changes` |
| `gnx_rename` | `commands::rename` |
| `gnx_route_map` | `commands::route_map` |
| `gnx_shape_check` | `commands::shape_check` |
| `gnx_multi_query` | `commands::multi_query` |

Rust MCP SDK: `rmcp` (Anthropic-blessed) primary candidate; `mcp-rs`
as fallback. Final pick deferred to implementation phase.

### Registration steps (what the TUI does)

1. Probe `claude --version` — abort with friendly message if not installed.
2. Atomic read-modify-write `~/.config/claude-code/mcp-servers.json`
   (or per-project `.mcp.json` if user picks project scope):
   ```json
   {
     "mcpServers": {
       "gnx": {
         "command": "gnx",
         "args": ["mcp", "serve"],
         "env": {}
       }
     }
   }
   ```
3. Print uninstall reminder: `gnx admin → Bind tool to code agent → MCP → Claude Code → uninstall`.

### Uninstall

Remove the `"gnx"` key from the same JSON file; leave other entries
untouched (read-modify-write, atomic).

### Status probe

`gnx admin` reads the JSON file and reports `installed` / `missing` /
`outdated` (compares the binary path in the JSON vs `which gnx-mcp`).

### Path 1b — Cursor / Windsurf / Cline / Roo Code / generic MCP host

All four of these (and any future MCP-capable host) consume the **same
`gnx-mcp` side-car binary** as Claude Code. The only difference is
where the registration entry lives:

| Host | Registration file | Format |
|---|---|---|
| Claude Code | `~/.config/claude-code/mcp-servers.json` or `.mcp.json` (project) | JSON `mcpServers` object |
| Cursor | `~/.cursor/mcp.json` or `.cursor/mcp.json` (project) | JSON `mcpServers` object (same shape) |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` | JSON |
| Cline / Roo Code | VS Code settings: `cline.mcpServers` | JSON inside `settings.json` |
| Other / unknown | TUI prints the JSON snippet + instructs user to paste into their host's config | Manual |

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
- `gnx` binary on PATH (since the Gemini-side tool wrapper invokes
  `gnx` as a subprocess — there's no shared Rust↔TS in-process path)

### File layout in the fork

```
packages/core/src/tools/
└── gnx/
    ├── index.ts          ← exports & registry hooks
    ├── context-tool.ts   ← class GnxContextTool extends BaseTool
    ├── impact-tool.ts    ← class GnxImpactTool extends BaseTool
    ├── query-tool.ts     ← class GnxQueryTool extends BaseTool
    └── ...               ← one file per gnx subcommand
```

Each tool class:
1. Declares schema via the existing `BaseTool` schema builder.
2. `execute()` spawns `gnx <subcmd>` with parsed args and returns the
   stdout/stderr as a `ToolResult`.

### Registry wiring

Edit `packages/core/src/tools/tool-registry.ts` (or whichever file
contains `registerBuiltinTools()`):

```typescript
import { GnxContextTool, GnxImpactTool, /* ... */ } from './gnx/index.js';

function registerBuiltinTools(registry: ToolRegistry) {
  // ... existing built-ins ...
  registry.register(new GnxContextTool());
  registry.register(new GnxImpactTool());
  // ...
}
```

### Distribution model

The TUI does NOT auto-fork or auto-patch the user's Gemini install.
Instead it:

1. Probes if `gemini --version` resolves.
2. Writes a patch file to `~/.config/gnx/host-integration/gemini-cli.patch`
   (template ships inside the `graph-nexus` crate as an embedded
   resource).
3. Prints the manual steps:
   ```
   cd <your gemini-cli fork>
   git apply ~/.config/gnx/host-integration/gemini-cli.patch
   npm run build
   npm link  # so the patched gemini takes precedence
   ```
4. Status probe: looks for the embedded marker string
   `// gnx-integration-marker-v1` in the user-supplied
   `gemini-cli` checkout (path provided in step 1).

### Maintenance burden disclosure

The TUI's install screen MUST display:
> "Forking Gemini CLI means re-applying this patch every time
> upstream gemini-cli releases. Expect ~10 minutes per upstream
> release. If that's too much overhead, use MCP integration instead."

## Path 3 — Codex CLI (Rust in-process workspace dep)

**Why this is the sweet spot for gnx**: Codex CLI is Rust. gnx is
Rust. The Codex CLI workspace can take a path/git dependency on the
`graph-nexus-cli` crate and call its `commands::*::run` functions
directly. No spawn, no IPC, no startup latency, mmap'd graph shared
across the same address space.

### Pre-reqs

- A local clone of [`openai/codex`](https://github.com/openai/codex)
- The user's fork remote
- This `graph-nexus-rs` repo cloned locally (or published to crates.io
  in a future iteration — current state: not yet on crates.io due to
  vendored grammars)

### Codex CLI fork modifications

`codex-rs/core/Cargo.toml`:
```toml
[dependencies]
graph-nexus-cli = { path = "../../../graph-nexus-rs/crates/graph-nexus-cli" }
graph-nexus-core = { path = "../../../graph-nexus-rs/crates/graph-nexus-core" }
```

`codex-rs/core/src/tools/gnx.rs` (new):
```rust
use graph_nexus_cli::commands::context;
use graph_nexus_cli::engine::Engine;

pub struct GnxContext;

impl Tool for GnxContext {
    fn name(&self) -> &str { "gnx_context" }

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
`~/.config/gnx/host-integration/codex-cli.patch` and prints manual
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
crates/graph-nexus-cli/src/commands/admin/
├── mod.rs              ← clap entrypoint + TUI bootstrap
├── menu.rs             ← top-level menu (ratatui or dialoguer)
├── host_integration/
│   ├── mod.rs
│   ├── claude_code.rs  ← MCP JSON read/write
│   ├── gemini_cli.rs   ← patch write + manual-step printer
│   └── codex_cli.rs    ← patch write + manual-step printer
└── ... (other admin areas, future)
```

`gnx admin` would be the only new top-level command added by this
spec. Its subcommands stay inside the TUI — never exposed as
`gnx admin install-claude-code` etc.

## Open questions (do not implement until resolved)

1. **Patch versioning**: how to keep the embedded Gemini/Codex patches
   in sync with upstream Gemini/Codex releases? Options: pin to a
   specific upstream commit in the patch header and warn if user's
   fork is ahead; generate the patch on demand from a template + the
   current gnx tool list.
2. **Tool naming collision**: if upstream gemini-cli or codex-cli adds
   a `gnx`-prefixed tool someday, our patch breaks. Mitigation: use
   `graph_nexus_<tool>` prefix instead of `gnx_<tool>`? The spec
   leaves this open.
3. **MCP server transport**: stdio is the simplest. Worth supporting
   HTTP+SSE later (for remote-graph use case)? Out of scope here.
4. **Capture-stdout vs `run_to_string`**: decide before Codex-CLI
   integration ships.
5. **Discoverability**: how does a user know `gnx admin` exists? README
   line + `gnx --help` mentions it. Both required.

## Non-goals (explicit)

- Auto-fork / auto-PR upstream gemini-cli / codex-cli on the user's behalf
- Cursor / Windsurf / ChatGPT integration (each requires a separate spec
  if it lands later — same TUI menu, separate handler)
- Self-hosted MCP transport (HTTP+SSE)
- VS Code / JetBrains plugin (different ecosystem entirely)

## Decision log

| Decision | Choice | Rationale |
|---|---|---|
| Top-level command name | `gnx admin` | Sibling to `gnx config`; "admin" signals "one-off setup" vs `config`'s "edit current settings". |
| Install command exposure | TUI-only, no flat `gnx install` | User constraint. Keeps `gnx` public surface minimal; install is admin work. |
| Menu top-level grouping | By **mechanism** (Native / MCP), not by host | User-clarified 2026-05-15: Codex and Gemini both support MCP *and* native; grouping by host would force a false either-or per host. Grouping by mechanism lets the user pick "how invasive" first and surfaces that MCP is the universal fallback for everything Anthropic-grade and below. |
| MCP side-car scope | One binary serves all MCP hosts | The `gnx` binary's `mcp serve` subcommand is host-agnostic — Claude Code, Cursor, Windsurf, Cline all consume the same stdio JSON-RPC interface. The TUI's only per-host work is writing to the right config file. |
| Binary layout | Single `gnx` binary; MCP via `gnx mcp serve` subcommand | User-clarified 2026-05-15: avoids second `gnx-mcp` install / PATH entry. Same binary, two execution modes. The `graph-nexus-mcp` crate provides the server library; `graph-nexus-cli` adds the `mcp` subcommand wrapper. |
| Args + business-logic sharing | Refactor `commands/<x>.rs::run` → `run_inner` (returns Value) + `run` (calls inner + emit) | User-clarified 2026-05-15: no schema or output duplication between CLI and MCP. Args struct adds `Serialize, Deserialize, JsonSchema` derives; schema auto-generated via `schemars::schema_for!`. MCP wrapper consumes `run_inner` directly. Precondition for graph-nexus-mcp crate work. |
| MCP output format | TOON by default (`ToolResult::text(toon_body)`) | User-clarified 2026-05-15: raw JSON would defeat the token-economy thesis. Reuse CLI's `emit_to_string(value, OutputFormat::Toon)` helper so MCP and CLI share serialization. Model can override via per-call `format` arg if it specifically needs JSON. |
| Output helper refactor | Add `output::emit_to_string()` returning String | Split serialization from stdout-write so both CLI (`emit`) and MCP (`ToolResult::text`) consume the same code. Paired with the `run_inner` refactor. |
| Native install automation | Manual (TUI prints patch + steps) | Too easy to corrupt user's git tree. Auto-`git apply` rejected. |
| MCP install automation | Fully automated (atomic JSON write) | All MCP host config files are well-known JSON; safe to auto-write with read-modify-write atomicity. TUI just asks "which host?" then writes the entry. |
| Native scope | Codex + Gemini only | User-clarified 2026-05-15: every other host is either closed source (Cursor/Copilot/Windsurf/Claude Code) or open source with too-small user base to justify per-fork maintenance (Cline/Roo/Aider/Continue). MCP catches all of those. |
| MCP host coverage | Claude Code, Cursor, Windsurf, Cline/Roo, Copilot Extensions, generic paste-it | Each gets a TUI leaf with the right config-file writer. Side-car binary identical across all. |
| Auto-patch user forks | No | Too easy to corrupt user's git tree. TUI writes patch file + prints manual steps. |
| Claude Code integration route | MCP only | Closed source — no other route exists. |
| Codex CLI integration route | Workspace dep (in-process) | Same language; zero IPC overhead; mmap'd graph shared. |
| Gemini CLI integration route | Subprocess via patch | Cross-language (TS↔Rust) — spawn `gnx` is the cleanest boundary. |
