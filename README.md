# EgentCodePlexus

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)
[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)

A code intelligence graph for **LLMs and AI code agents** — one-shot CLI, zero-copy mmap, sub-second per query.

[繁體中文 (Traditional Chinese)](./docs/readme_i18n/README_zh-TW.md)

---

## 🎯 Mission

`ecp` exists to be the structural-knowledge layer that an autonomous AI coding agent calls 20–50 times per task. Every design decision falls out of that one premise:

- **Built for agents, not IDEs.** Output is token-cheap (TOON / compact JSON), every flag surfaces via `--help`, every command is non-interactive and stdout-parseable. No UI, no human-skim layout cruft eating the agent's context window.
- **No warm-up, no daemon.** Each invocation `mmap`s a zero-copy `rkyv` graph file and exits. Read queries return in **~140–170 ms** *including process startup*; a 22k-file repo cold-indexes in under 3 s. An agent can fire dozens of queries per task without amortising a server boot, and there is no "daemon died, please restart" failure mode.
- **Honest answers over readable graphs.** When a call site can't be statically resolved (dynamic dispatch, unresolved import, reflection), `ecp` emits a `BlindSpot` record — not a guessed edge. An agent that acts on a hallucinated dependency is much more expensive than one that gets an "I don't know" it can route around.
- **Polyglot reach.** 31 languages parsed at the structural level so modern multi-stack repos (service code + Dockerfiles + GitHub Actions + Terraform + SQL + smart contracts) stop being black holes the moment you leave the main language.

🎙️ **[Agent Interviews](./interviews/README.md)** — See how real AI agents (Gemini CLI, Codex) use and evaluate `ecp` in autonomous workflows.

Built on top of [GitNexus](https://github.com/abhigyanpatwari/GitNexus) by [Abhigyan Patwari](https://github.com/abhigyanpatwari) — same conceptual model (a structural knowledge graph of a repo), rewritten in Rust for a different audience. Licensed under [PolyForm Noncommercial 1.0.0](./LICENSE.md); see [NOTICES.md](./LICENSES/NOTICES.md) for required attribution.

---

## ⚡ Performance

The Mission section above is *why* `ecp` is built the way it is. This section is the receipts.

### Head-to-head vs. upstream GitNexus

Measured on the [gitnexus](https://github.com/abhigyanpatwari/GitNexus) codebase (TypeScript) using `scripts/parity/benchmark_vs_gitnexus.py`:

| Phase | ecp (Rust) | gitnexus (Node) | Speedup |
|---|---|---|---|
| **Cold Index** | **~970 ms** | ~58 s | **60×** |
| **Symbol Context** | **~70 ms** | ~430 ms | **6×** |
| **Blast Radius** | **~70 ms** | ~460 ms | **6×** |
| **Cypher Query** | **~70 ms** | ~400 ms | **5×** |

*Note: `ecp` query latency includes full process startup (no daemon). GitNexus (v1.6.5) query latency is against a warm, indexed repo via its CLI.*

### Scalability — single run on `.sample_repo` (a 2.1 GB polyglot collection of ~40 real-world open source projects across 25+ languages, used for cross-language stress testing)

**Ingest performance:**

| Phase | Value |
|---|---|
| Files indexed | **22,645** across 25 detected languages |
| Wall-clock (Cold) | **2.60 s** (parse + resolve + serialize) |
| Wall-clock (Incremental) | **4.9 ms** (xxh3_64 hash walk, zero dirty files) |
| Hardware | AMD Ryzen 9 9950X (16 logical), 39.2 GiB RAM, Linux 6.6.87 |

**Per-query latency (including process startup):**

| Query | Median | Notes |
|---|---|---|
| `summary` (registry overview) | **1.4 ms** | smallest read — just registry mmap |
| `routes` (HTTP route map across repo) | **142.3 ms** | enumerates declarative + imperative |
| `summary --detailed` (frameworks + blind-spots) | **143.4 ms** | full registry + per-framework scoring |
| `impact <symbol> --direction down` | **145.0 ms** | BFS over Calls / Extends edges |
| `inspect <symbol>` (signature + callers + callees) | **145.6 ms** | symbol resolution + 1-hop traversal |
| `find <name> --mode bm25` (lexical search) | **154.5 ms** | Tantivy query + 5-bucket partition |
| `cypher 'MATCH (a:Class)-[:HasMethod]->(b:Method) ...'` | **161.5 ms** | one pattern, one row returned |
| `cypher 'MATCH (a:Method)-[:Calls]->(b:Method) ...'` | **174.2 ms** | broader pattern, more matches |
| `impact --baseline HEAD~1` (change-set blast radius) | **359.0 ms** | git diff + parallel per-file parse + BFS |

Reproduce: `python scripts/benchmark/benchmark_ecp.py`.

---

## vs. upstream GitNexus

Same conceptual model, different audience. `ecp` is **not** a drop-in replacement — choose based on who reads the graph and what they do with it.

| Dimension | EgentCodePlexus | GitNexus |
|---|---|---|
| Primary consumer | Autonomous AI code agents | Human devs + IDE integration |
| Runtime | Stateless one-shot CLI (zero warm-up) | Long-running MCP server |
| Performance | **< 2.5s cold index / < 150ms query** | ~60s cold index / ~400ms query |
| Unresolved edge | `BlindSpot` record (honest unknown) | Heuristic guess |
| Default output | TOON / compact JSON (token-cheap) | Wiki / UI rendering |
| Languages | 31 (14 deep + 17 structural) | 14 (deep, 9-dimension) |
| Storage | Rust + `rkyv` zero-copy mmap | Node.js + LadybugDB |

**Full breakdown of all 8 dimensions, philosophy, and decision matrix → [docs/vs-gitnexus.md](./docs/vs-gitnexus.md)**

---

## 📦 Install

Prebuilt binaries are published with each GitHub Release. The installer scripts fall back to a cargo source build only when a matching release asset is unavailable.

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# Explicit cargo path (same source build, no installer wrapper)
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

Optional CPU-tuned source build:

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

---

## 🚀 Quick start

```bash
# 1. Index the current repo (incremental; first query also auto-indexes)
ecp admin index --repo .

# 2. Locate a symbol — exact name by default
ecp find loginUser
ecp find login --mode bm25       # ranked BM25, top-K partitioned by source/tests/ref/doc/config

# 3. Blast radius — who breaks if I change this?
ecp impact validateUser --direction upstream

# 4. Full symbol context (signature, body, callers, callees, 1-hop impact)
ecp inspect validateUser

# 5. Every HTTP route in the repo (declarative @Get + imperative app.get())
ecp routes
ecp routes /api/users --method POST     # route → handler → caller chain
```

Read-side commands accept `--format text|json|toon`. Default per command is the token-cheapest representation (mostly `toon`; `find` defaults to `text`; `cypher`/`summary` default to `json`).

---

## CLI surface

Two tiers — **agent commands** at top level (query/refactor/verify) and **admin commands** under `ecp admin` (registry/hooks/destructive). Run `ecp --help` and `ecp admin --help` for full flag matrices.

| Command | Purpose |
|---|---|
| `inspect <name>` | One symbol → metadata, decorators, signature, callers, callees, 1-hop impact |
| `find <pattern>` | Locate symbols — exact (default) · `--mode fuzzy` substring · `--mode bm25` lexical ranking; bm25 partitions output into source / tests / reference / document / config buckets |
| `find-schema-bindings <field>` | Surface MirrorsField heuristic edges + blind-spot candidates (schema field mirrors across classes / services). Format: toon (default) or json. |
| `find-transaction-patterns [--class <Name>]` | Detect Saga compensate/undo/rollback name-pairs on same class. Confidence ≥0.75 tier:POSSIBLY_RELATED, <0.75 tier:BLIND_SPOT. Outbox half deferred (T5-33). |
| `impact <name> --direction <up\|down>` | Blast-radius traversal with confidence filtering. `--since <ref>` for change-set impact. |
| `rename --symbol <old> --new-name <new>` | AST-aware multi-file rename across 14 languages. Always `--dry-run` first. |
| `cypher '<query>'` | openCypher escape hatch; `m.content` returns source body. |
| `summary` | Registry overview, framework coverage, LLM-actionable blind-spot catalog, graph freshness. (Was `coverage`; the old verb still works as an alias.) |
| `routes [<path>]` | Enumerate HTTP routes (declarative + imperative); with `<path>` show handler + callers. |
| `contracts` | Cross-repo API contract inventory (routes / queue / RPC). |
| `diff` | Resolver-delta — edge-level binding tier-degradation + route / contract changes. |
| `tool-map` | Calls to external HTTP / DB / Redis / queue clients via per-file import-binding analysis. |
| `shape-check` | Drift between HTTP consumer access patterns and Route response shapes. |
| `peers` | Multi-session peer collaboration: `status` / `diff` / `say` / `inbox` / `log` / `thread` / `watch` / `gc`. See [Multi-session peer sync](#multi-session-peer-sync). |
| `review` | Aggregated LLM-workflow audit: runs impact + coverage + tool-map + shape-check + diff in one shot, filtered to high-confidence signals. |

Admin namespace (`ecp admin <cmd>` — hidden from top-level help):

| Command | Purpose |
|---|---|
| `index --repo <path>` | Build / refresh the graph; incremental via xxh3_64 content cache. `--force` for full rebuild. |
| `drop / prune / rename-branch` | Index lifecycle: delete, prune stale branch dirs, rename branch on-disk. |
| `install-hook` | Install the git reference-transaction hook (auto-track branch switches). |
| `config` | Interactive TOML wizard for `.ecp/config.toml`. |
| `mcp serve` / `mcp tools` | MCP server (stdio) for LLM hosts; `tools` lists the exposed tool surface. |

All commands resolve `.ecp/graph.bin` from CWD unless `--graph <path>` is given. Agent-facing commands are non-interactive by design — every flag surfaces via `--help`, every output stream is parseable.
Run `ecp admin` with no subcommand to open the interactive admin TUI for index maintenance, host integrations, config, groups, and diagnostics.

### Multi-session peer sync

When two or more LLM sessions edit the same repo in parallel (e.g. one Claude Code session per feature branch), `ecp peers` lets each session see what the others are touching and exchange short messages so they don't trample each other's symbol-level dirty surface.

Each session must register itself by passing a stable session id through one of these env vars (whichever your host populates): `ECP_SESSION_ID`, `CODEX_SESSION_ID`, `CODEX_THREAD_ID`, or `CLAUDE_CODE_SESSION_ID`. The host's session-start hook normally does this for you.

```bash
# 1. Start the inotify watcher daemon (one per session, detached). Required for
#    peer dirty-event dispatch into your inbox; status / say / inbox / log all
#    work without it but you won't get auto-notified when a peer edits.
ecp peers watch --start

# 2. List live peers — who else is editing this repo right now.
ecp peers status                 # text
ecp peers status --format json   # array of { session_id, pid, watcher, … }
                                 #  watcher ∈ alive | dead | not-started

# 3. Inspect a peer's symbol-level dirty surface (optionally filter by symbol).
ecp peers diff <peer-session-id> [<symbol-name>]

# 4. Send / receive short messages. Broadcast (no --to) writes to every alive
#    peer's inbox; targeted writes to one inbox.
ecp peers say "rebasing on main, hold pushes for 5min"
ecp peers say --to <peer-session-id> "can you take review on auth.rs?"
ecp peers inbox                  # read own inbox without draining
ecp peers log --limit 20         # tail this session's msg.log
ecp peers thread <msg-id>        # all messages threaded by msg_id

# 5. Stop the watcher when done. `gc` rotates log files.
ecp peers watch --stop
ecp peers gc
```

The watcher status field distinguishes `not-started` (you never ran `watch --start`) from `dead` (it was running but the pid is gone — crashed or killed), so failures don't masquerade as "feature not used".

### Provable verdicts (LLM code review)

`ecp review --verdicts` emits a pre-computed set of provable code-review verdicts derived from `ecp diff` sections. Instead of asking an LLM to re-derive caller relationships from a raw diff, hand the JSON output directly as review context.

```bash
ecp review --since main --verdicts --format json
```

**Severity model:**

| Severity | Rule |
|---|---|
| `RISK` | Cross-file callers exist (symbol imported by other modules), or public symbol was removed, or blindspot in modified file |
| `WARN` | Intra-file callers only (one file), or route modified |
| `INFO` | No callers found, or new public surface added |

**Verdict kinds:**

- `SIGNATURE_OR_BODY_CHANGED` — symbol's source changed; severity escalates by caller reachability
- `NEW_PUBLIC_SURFACE` — public-level symbol added (Function / Method / Class / Struct / Enum / Trait / Route / EventTopic / SchemaField)
- `REMOVED_PUBLIC_SURFACE` — public-level symbol deleted (always RISK)
- `ROUTE_CONTRACT_CHANGED` — HTTP route added / removed / modified
- `BLINDSPOT_IN_DIFF_REGION` — eval / dynamic dispatch / reflection inside changed file

Every verdict cites the exact diff section and graph fact that triggered it; see [Provable Verdicts Design Spec](./docs/specs/2026-05-22-review-verdicts.md) for semantics, provability invariants, and deferred features.

---

## MCP server (for LLM hosts)

`ecp` ships an MCP server exposing core commands as MCP tools. Hosts that speak MCP (Claude Code, Cursor, Windsurf, Cline, Codex CLI, Gemini CLI) can register `ecp` and call the tools autonomously.

```bash
ecp admin mcp tools          # inspect what tools will be exposed
ecp admin mcp serve          # run the server (default: spawn mode, fresh subprocess per call)
```

Manual host config example for Claude Code (`~/.config/claude-code/mcp-servers.json`):

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

Progressive path for human operators:

```text
ecp admin
→ Agent Integrations
→ MCP
→ <host>
→ install
```

## Codex CLI native integration

The Codex native path is separate from MCP. It prepares a patch for an `openai/codex` fork instead of editing the running Codex installation directly:

Progressive path for human operators:

```text
ecp admin
→ Agent Integrations
→ Codex CLI
→ install
→ native-tools
```

Bundled skills use the same progressive path:

```text
ecp admin
→ Agent Integrations
→ Codex CLI
→ install
→ skills
→ all | ecp | simplify
```

Scripted path for AI agents and automation:

```bash
ecp admin codex install native-tools
ecp admin codex install skills all
ecp admin codex install skills ecp
ecp admin codex install skills simplify
```

The bundled skills teach workflow selection that command help cannot infer by itself:

| Skill | Use when |
|---|---|
| `ecp` | The agent needs to decide whether graph-aware symbol, impact, route, contract, or rename workflows are better than grep / file reads. |
| `simplify` | The agent is reviewing changed code and should start from ecp impact, blind spots, egress, shape drift, and resolver deltas before reading raw diffs. |

The `native-tools` component writes:

```text
~/.config/ecp/host-integration/codex-cli.patch
```

Apply the patch in your Codex CLI fork, then wire the generated module into Codex's tool registry:

```bash
cd /path/to/openai-codex-fork
git apply ~/.config/ecp/host-integration/codex-cli.patch
```

To verify a fork that already has the native marker, set `ECP_CODEX_CLI_CHECKOUT` before checking status in the TUI:

```bash
ECP_CODEX_CLI_CHECKOUT=/path/to/openai-codex-fork ecp admin
# Agent Integrations → Codex CLI → status
```

The equivalent scripted checks are:

```bash
ECP_CODEX_CLI_CHECKOUT=/path/to/openai-codex-fork ecp admin codex status
ecp admin codex uninstall native-tools
ecp admin codex uninstall skills all
```

---

## Architecture

```
crates/
├── ecp-core        # Zero-copy graph (rkyv + mmap), incremental cache, graph queries
├── ecp-analyzer    # Tree-sitter parsers, HTTP route detector, framework confidence
├── ecp-mcp         # MCP server (stdio) — exposes core commands as tools
└── ecp-cli         # `ecp` binary, Tantivy BM25 engine, token-optimized output
```

Parse → resolve → serialize runs through an MPSC channel into a single builder thread that assembles the graph and writes a zero-copy `.ecp/graph.bin`. Read paths (`inspect`, `cypher`, `impact`, …) mmap this file directly. The xxh3_64 content cache keeps incremental rebuilds at sub-second on a 22k-file repo.

---

## Language coverage

31 languages parsed at the structural level (functions / classes / methods / imports / calls). 14 of them — the original GitNexus set — get full-depth coverage across imports, named bindings, exports, heritage, types, constructors, config, frameworks, entry points, calls, and rename. The remaining 17 are structural-only (Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig).

📊 **[Full Language Capability Matrix](./docs/language-matrix.md)** — Detailed per-language status and rationale.

---

## ⚙️ Tuning

| Env var | Default | Effect |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216` (16 MiB) | Skip source files larger than this during ingest. Caps worst-case worker RAM at `num_threads × MAX`. |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | Directory recursion depth for `*.csproj` discovery. Raise for deeply-nested .NET monorepos. |

---

## License & acknowledgments

Licensed under [PolyForm Noncommercial 1.0.0](./LICENSE.md). Personal use, research, hobby projects, and noncommercial organizations are explicitly permitted. **Commercial use is not granted by this license** — contact the upstream GitNexus author Abhigyan Patwari for commercial rights.

Built on:
- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — original design, CLI surface, and conceptual model
- [tree-sitter](https://tree-sitter.github.io/) — robust incremental AST parsing
- [rkyv](https://rkyv.org/) — zero-copy deserialization framework
- [Tantivy](https://github.com/quickwit-oss/tantivy) — blazing fast Rust full-text search engine
- [Rayon](https://github.com/rayon-rs/rayon) — data parallelism for multi-core concurrent AST parsing
- [xxhash (xxh3_64)](https://xxhash.com/) — extremely fast non-cryptographic hashing for content-based incremental indexing
- [DashMap](https://github.com/xacrimon/dashmap) — high-performance concurrent hash maps for graph assembly
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — zero-copy memory mapping for sub-millisecond graph access
- [msgspec](https://github.com/jcrist/msgspec) — high-performance JSON serialization for inter-process communication

Onboarding for AI agents (URL bootstrap, Claude Code skill, plugin install) lives at `docs/skills/ecp-onboard/`. Concurrency invariants and how to re-verify them: `./scripts/audit/audit-concurrency.sh`.

## Release status

The current verified install path is `cargo install --git ...`, which builds `ecp` from source. Release installers already contain the checksum and provenance-verification flow, but they require a published tag and release assets before the binary download path can be end-to-end verified. The agent-facing onboarding skill is documented in [docs/skills/ecp-onboard/ONBOARDING.md](./docs/skills/ecp-onboard/ONBOARDING.md); it is intended to guide users through install, first index, optional groups, MCP wiring, and next steps. The assisted configuration/setup flow is still being refined.
