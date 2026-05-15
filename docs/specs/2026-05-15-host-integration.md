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
- The TUI menu structure (target):
  ```
  gnx admin
   ├── Host integration
   │   ├── Claude Code (MCP)        → install / uninstall / status
   │   ├── Gemini CLI (fork-patch)  → install / uninstall / status
   │   └── Codex CLI (workspace-dep)→ install / uninstall / status
   ├── (future) Diagnostics
   └── (future) Index maintenance
  ```
- Status detection: each host's `status` action probes whether the
  current install is present (file exists / config entry present /
  workspace dep declared) and reports `installed` / `missing` /
  `outdated`.

## Path 1 — Claude Code (MCP server)

**Why MCP for Claude Code**: Claude Code is closed source. There is no
public extension point that injects a tool into the model's
first-class tool list other than MCP. Skills / hooks / CLAUDE.md are
prompt-layer mechanisms; the model sees them as text, not tools.

### Pre-reqs

- Claude Code installed (`claude` on PATH)
- `gnx-mcp` binary on PATH (to be built — see below)

### Build target

A new crate `crates/graph-nexus-mcp/` exposes a stdio JSON-RPC MCP
server. It wraps a subset of `graph-nexus-cli::commands::*` as MCP
tools. Initial tool set (mirrors upstream gitnexus naming for
familiarity):

| MCP tool | Wraps |
|---|---|
| `gnx_context` | `commands::context::run` |
| `gnx_impact` | `commands::impact::run` |
| `gnx_query` | `commands::query::run` |
| `gnx_detect_changes` | `commands::detect_changes::run` |
| `gnx_rename` | `commands::rename::run` |
| `gnx_route_map` | `commands::route_map::run` |
| `gnx_shape_check` | `commands::shape_check::run` |
| `gnx_multi_query` | `commands::multi_query::run` |

Rust MCP SDK: `rmcp` (or `mcp-rs`) — final pick deferred to
implementation phase.

### Registration steps (what the TUI does)

1. Probe `claude --version` — abort with friendly message if not installed.
2. Write `~/.config/claude-code/mcp-servers.json` (or per-project
   `.mcp.json` if user picks project scope) entry:
   ```json
   {
     "mcpServers": {
       "gnx": {
         "command": "gnx-mcp",
         "args": [],
         "env": {}
       }
     }
   }
   ```
3. Print uninstall reminder: `gnx admin → Host integration → Claude Code → uninstall`.

### Uninstall

Remove the `"gnx"` key from the same JSON file; leave other entries
untouched (read-modify-write, atomic).

### Status probe

`gnx admin` reads the JSON file and reports `installed` / `missing` /
`outdated` (compares the binary path in the JSON vs `which gnx-mcp`).

## Path 2 — Gemini CLI (fork patch)

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
| Auto-patch user forks | No | Too easy to corrupt user's git tree. TUI writes patch file + prints manual steps. |
| Claude Code integration route | MCP only | Closed source — no other route exists. |
| Codex CLI integration route | Workspace dep (in-process) | Same language; zero IPC overhead; mmap'd graph shared. |
| Gemini CLI integration route | Subprocess via patch | Cross-language (TS↔Rust) — spawn `gnx` is the cleanest boundary. |
