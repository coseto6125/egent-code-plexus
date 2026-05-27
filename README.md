<div align="center">

# `ecp` · EgentCodePlexus

### The structural code graph built for AI agents, not humans.

*22k files indexed in 2.6 s · any query answered in &lt;175 ms · honest unknowns, never hallucinated edges.*

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)
![Cold index 2.6s](https://img.shields.io/badge/cold_index-2.6s%20%2F%2022k%20files-brightgreen)
![Query latency](https://img.shields.io/badge/query-%3C175ms%20cold-blue)
![Languages](https://img.shields.io/badge/languages-31%20parsed-orange)
![License](https://img.shields.io/badge/license-PolyForm%20NC-lightgrey)
![Built with Rust](https://img.shields.io/badge/built_with-Rust-orange?logo=rust)
![Status early release](https://img.shields.io/badge/status-early%20release-yellow)

**English** · [繁體中文](./docs/readme_i18n/README_zh-TW.md) · [简体中文](./docs/readme_i18n/README_zh-CN.md) · [日本語](./docs/readme_i18n/README_ja.md) · [한국어](./docs/readme_i18n/README_ko.md) · [Español](./docs/readme_i18n/README_es.md) · [Português](./docs/readme_i18n/README_pt-BR.md) · [Русский](./docs/readme_i18n/README_ru.md) · [हिन्दी](./docs/readme_i18n/README_hi.md)

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh
```

[All install options](#-install) · [Uninstall](#uninstall)

</div>

---

Autonomous coding agents fire **20–50 structural queries per task**. Those queries all hit tools built for humans: IDE sidebars, daemons that need warming, output formatted for reading. The mismatch shows up in three concrete failure modes:

1. **Token waste** — a `grep` dump returns 400 lines where the agent needed 10 symbols
2. **Broken refactors** — a missed caller slips through because the resolver guessed and got it wrong
3. **Hallucinated dependencies** — when static analysis can't reach an edge, the tool invents one

`ecp` was built to eliminate all three.

| Failure mode | `ecp`'s answer |
|---|---|
| Context window blown on raw search output | **TOON / compact JSON** — symbols, lines, and edges only; no padding |
| Missed caller, silent downstream breakage | **`impact`** — exact blast radius over real call and extend edges |
| Fabricated dependency in the agent's reasoning | **`BlindSpot` records** — typed honest unknowns the agent can route around |
| Graph goes dark outside the primary language | **31 languages** — service code, IaC, SQL, smart contracts in one traversal |

---

## 🎯 Design principles

Each design decision has one source: *what does the receiving agent actually need?*

**Output is a data structure.** TOON and compact JSON carry only what the agent needs for its next decision. No prose summaries. No visual chrome. No section headers consuming the context budget. The format defaults are already the right choice for most LLM prompts.

**Stateless. Zero warm-up.** Each invocation `mmap`s a zero-copy `rkyv` graph file and exits. **~140–170 ms per query, startup included.** No daemon to keep alive. No warm-up phase. No "server crashed, please restart" recovery path. An agent can fire 50 queries per task without paying a process boot cost.

**BlindSpot over hallucination.** When `ecp` can't statically resolve a call site — dynamic dispatch, reflection, an unresolved import — it emits a `BlindSpot` record: a named, typed, explicit gap in the graph. Agents can navigate around a known unknown. They cannot recover from a confident fabrication.

**Polyglot by default.** 31 languages at structural depth. Service code, Dockerfiles, GitHub Actions, Terraform, SQL, Move, Solidity — one traversal covers all layers. No language switch means no graph blind spot.

🎙️ **[Agent Interviews](./interviews/README.md)** — Gemini CLI and Codex describe how they use `ecp` in live autonomous task flows.

Built on [GitNexus](https://github.com/abhigyanpatwari/GitNexus) by [Abhigyan Patwari](https://github.com/abhigyanpatwari) — same structural-graph concept, rewritten in Rust, different audience. [PolyForm Noncommercial 1.0.0](./LICENSE.md); see [NOTICES.md](./LICENSES/NOTICES.md) for required attribution.

---

## ⚡ Performance receipts

Head-to-head against two other code-graph tools — [`codegraph`](https://github.com/colbymchenry/codegraph) (Node + SQLite) and upstream [`gitnexus`](https://github.com/abhigyanpatwari/GitNexus) (Node) — on the same checkouts, same machine. `ecp` is a stateless one-shot CLI: every latency below **includes full process startup**, no daemon, no warm-up.

*Versions: `ecp` 0.4.2 · `codegraph` 0.9.4 · `gitnexus` 1.6.5. All tools capped at a 1 MiB max-file-size threshold where configurable (`gitnexus` hard-codes 512 KB). `ecp` medians over 5–7 runs. Hardware: AMD Ryzen 9 9950X (16 logical), Linux.*

### `microsoft/vscode` — 14,874 files, dense single-language TypeScript

| Metric | **`ecp`** | `codegraph` | `gitnexus` |
|---|---|---|---|
| **Cold index** | **4.6 s** | 166.9 s | **DNF** — killed at 27 min |
| Peak RSS | **~1.0 GiB** | 1.7 GiB | 4.6 GiB (still climbing) |
| Symbol find / query | **34.6 ms** | 169.5 ms | — |
| Callers / impact | **27.2 ms** | 172.4 ms | — |
| Inspect / context | **35.0 ms** | 415.9 ms | — |
| Impact baseline (git-diff) | **725.9 ms** | N/A — no such mode | — |
| Graph nodes | **507,257** | 315,498 | — |
| Graph edges | 916,380 | **986,709** | — |
| Index size on disk | **87 MiB** | 671 MiB | — |
| Files indexed | **14,874** | 10,814 | — |

*`gitnexus` did not finish — killed after 27 min stuck in its in-memory graph-resolution phase (RSS 4.6 GiB, no output written).*

### `abhigyanpatwari/GitNexus` — 3,232 files, polyglot (the corpus all three can finish)

| Metric | **`ecp`** | `codegraph` | `gitnexus` |
|---|---|---|---|
| **Cold index** | **0.74 s** | 11.2 s | 77.6 s |
| Peak RSS | **264 MiB** | 501 MiB | 2.5 GiB |
| Find / query | **9.4 ms** | 103.5 ms | — |
| Callers / impact | **9.2 ms** | 104.2 ms | 297.6 ms |
| Inspect / context | **9.4 ms** | — | 295.5 ms |
| Graph nodes | **49,122** | 19,604 | 30,223 |
| Graph edges | **48,271** | 39,155 | 47,218 |
| Index size on disk | **7.7 MiB** | 37 MiB | 306 MiB |
| Files indexed | **3,232** | 2,968 | 3,232 |

**Cold index: 15–37× faster than `codegraph`; `gitnexus` doesn't finish on a real large repo. Lowest memory, smallest on-disk index, densest graph — at every scale.**

### Scale: `.sample_repo` — 22,645 files, 25 languages, 2.1 GB polyglot corpus

**Ingest:**

| Metric | Value |
|---|---|
| Files indexed | **22,645** across 25 detected languages |
| Cold ingest | **2.60 s** (parse + resolve + serialize) |
| Incremental ingest | **4.9 ms** (xxh3_64 hash walk, zero dirty files) |
| Hardware | AMD Ryzen 9 9950X (16 logical), 39.2 GiB RAM, Linux 6.6.87 |

**Per-query latency, process startup included:**

| Query | Median | What it covers |
|---|---|---|
| `summary` | **1.4 ms** | registry mmap — smallest read |
| `routes` | **142.3 ms** | declarative + imperative route enumeration |
| `summary --detailed` | **143.4 ms** | full registry + per-framework confidence scoring |
| `impact --direction down` | **145.0 ms** | BFS over Calls / Extends edges |
| `inspect` | **145.6 ms** | symbol resolution + 1-hop traversal |
| `find --mode bm25` | **154.5 ms** | Tantivy query + 5-bucket partition |
| `cypher` (narrow) | **161.5 ms** | one pattern, one row |
| `cypher` (broad) | **174.2 ms** | wider pattern, more matches |
| `impact --baseline HEAD~1` | **359.0 ms** | git diff + parallel per-file parse + BFS |

Reproduce everything: `python scripts/benchmark/benchmark_ecp.py`.

### Rust-tier competitor comparison

`scripts/benchmark/benchmark_vs_competitors.py` benchmarks against [`codescope`](https://github.com/onur-gokyildiz-bhi/codescope) (SurrealDB-backed) and `coraline` (SQLite-backed) across 6 phases: `cold-index`, `symbol-find`, `callers`, `file-context`, `route-map`, `cypher`. Missing phases → `N/A` (absence is signal). Results regenerate `docs/benchmark-vs-competitors.md`.

```bash
python scripts/benchmark/benchmark_vs_competitors.py
python scripts/benchmark/benchmark_vs_competitors.py --corpus path/to/repo --iterations 5 --no-plot
```

---

## 🆚 vs. upstream GitNexus

Same structural-graph concept, different audience. Not a drop-in replacement — choose based on who reads the output and what they do with it.

| Dimension | EgentCodePlexus | GitNexus |
|---|---|---|
| Primary consumer | Autonomous AI code agents | Human devs + IDE integration |
| Runtime | Stateless one-shot CLI (zero warm-up) | Long-running MCP server |
| Performance | **< 2.5s cold index / < 175ms query** | ~60s cold index / ~400ms query |
| Unresolved edge | `BlindSpot` record (honest unknown) | Heuristic guess |
| Default output | TOON / compact JSON (token-cheap) | Wiki / UI rendering |
| Languages | 31 (14 deep + 17 structural) | 14 (deep, 9-dimension) |
| Storage | Rust + `rkyv` zero-copy mmap | Node.js + LadybugDB |

**Full breakdown, philosophy, and decision matrix → [docs/vs-gitnexus.md](./docs/vs-gitnexus.md)**

---

## 📦 Install

Prebuilt binaries ship with each GitHub Release. Installer scripts fall back to a cargo source build only when a matching asset is unavailable.

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# Direct cargo (no installer wrapper)
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

Prefer a package manager? The npm and PyPI packages ship the same prebuilt
binary (no compile, no toolchain) and pick the right platform automatically:

```bash
# npm — run without installing, or install globally
npx egent-code-plexus --help
npm install -g egent-code-plexus

# PyPI — via uv or pipx
uvx egent-code-plexus --help
uv tool install egent-code-plexus     # or: pipx install egent-code-plexus

# cargo-binstall (prebuilt, no source build)
cargo binstall egent-code-plexus
```

CPU-tuned source build:

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

### Uninstall

```bash
ecp uninstall            # remove agent integrations + ~/.ecp cache + the binary
ecp uninstall --dry-run  # preview what would be removed, change nothing
```

One command reverses every setup side-effect: Claude Code / Codex / Gemini
hooks, MCP servers, and skills; the per-repo git hook; the `~/.ecp` index cache;
and the `ecp` binary itself. On Windows the binary is deleted by a short delayed
step after the process exits (a running `.exe` can't delete itself in place).

Scope it to one agent with `--agent claude` (leaves the binary and cache in
place), or keep the index cache across a reinstall with `--keep-cache`. If you
installed via a package manager, use its own remover instead:

```bash
npm uninstall -g egent-code-plexus
uv tool uninstall egent-code-plexus   # or: pipx uninstall egent-code-plexus
cargo uninstall egent-code-plexus
```

---

## 🚀 Quick start

No daemon to start. No config required. One command from zero to a queryable graph.

```bash
# Index (incremental; first query also auto-indexes if index is absent)
ecp admin index --repo .

# Find a symbol — exact by default
ecp find loginUser
ecp find login --mode bm25            # BM25 ranking, partitioned into 5 output buckets

# Blast radius — who breaks if I change this?
ecp impact validateUser --direction upstream

# Full symbol context (signature, body, callers, callees, 1-hop impact)
ecp inspect validateUser

# HTTP route map (declarative @Get + imperative app.get())
ecp routes
ecp routes /api/users --method POST   # route → handler → caller chain

# File usage: who reads / writes this path?
ecp impact --literal session_meta.json
```

All read-side commands accept `--format text|json|toon`. Defaults are token-cheapest per command (mostly `toon`; `find` defaults to `text`; `cypher`/`summary` default to `json`).

---

## 🛠️ CLI surface

Two tiers: **agent commands** at top level (query / refactor / verify) and **admin commands** under `ecp admin` (registry / hooks / destructive). Run `ecp --help` and `ecp admin --help` for full flag matrices.

**Agent commands:**

| Command | Purpose |
|---|---|
| `inspect <name>` | Symbol → metadata, decorators, signature, callers, callees, 1-hop impact, contained methods / properties / enum variants |
| `find <pattern>` | Exact · `--mode fuzzy` · `--mode bm25` (5 buckets: source / tests / reference / document / config) |
| `find-schema-bindings <field>` | MirrorsField heuristic edges + blind-spot candidates across classes / services |
| `find-transaction-patterns [--class <Name>]` | Saga compensate/undo/rollback name-pairs; ≥0.75 → POSSIBLY_RELATED, <0.75 → BLIND_SPOT |
| `impact <name> --direction <up\|down>` | Blast-radius BFS with confidence filtering; `--since <ref>` for change-set impact |
| `rename --symbol <old> --new-name <new>` | AST-aware multi-file rename across 14 languages. Always `--dry-run` first. |
| `cypher '<query>'` | openCypher escape hatch; `m.content` returns source body |
| `summary` | Registry overview, framework coverage, LLM-actionable blind-spot catalog, graph freshness |
| `routes [<path>]` | HTTP route enumeration (declarative + imperative); with `<path>`: handler + caller chain |
| `contracts` | Cross-repo API contract inventory (routes / queue / RPC) |
| `diff` | Resolver-delta: binding tier-degradation + route / contract changes |
| `tool-map` | External HTTP / DB / Redis / queue call sites via import-binding analysis |
| `shape-check` | Drift between HTTP consumer access patterns and Route response shapes |
| `peers` | Multi-session collaboration: `status / diff / say / inbox / log / thread / watch / gc` |
| `review` | One-shot audit: impact + summary + tool-map + shape-check + diff, high-confidence signals only |

**Admin commands** (`ecp admin <cmd>`):

| Command | Purpose |
|---|---|
| `index --repo <path>` | Build / refresh the graph; incremental via xxh3_64 content cache. `--force` for full rebuild. |
| `drop / prune / rename-branch` | Index lifecycle: delete, prune stale branch dirs, rename branch on-disk |
| `install-hook` | Git reference-transaction hook (auto-tracks branch switches) |
| `config` | Interactive TOML wizard for `.ecp/config.toml` |
| `mcp serve` / `mcp tools` | MCP server (stdio); `tools` lists exposed surface |

All commands resolve `.ecp/graph.bin` from CWD unless `--graph <path>` is given. Every agent-facing command is non-interactive; every output stream is parseable.

### Multi-session peer sync

When multiple LLM sessions edit the same repo in parallel, `ecp peers` surfaces each session's symbol-level dirty state and enables direct session messaging. Register via `ECP_SESSION_ID`, `CODEX_SESSION_ID`, `CODEX_THREAD_ID`, or `CLAUDE_CODE_SESSION_ID`.

```bash
# Start the watcher (one per session; required for inbox push events)
ecp peers watch --start

# Who else is editing right now?
ecp peers status                                  # text
ecp peers status --format json                    # {session_id, pid, watcher: alive|dead|not-started}

# Inspect a peer's dirty symbols
ecp peers diff <peer-session-id> [<symbol>]

# Send messages
ecp peers say "rebasing on main, hold pushes 5min"    # broadcast
ecp peers say --to <peer-session-id> "take auth.rs?"  # targeted

# Read and manage
ecp peers inbox
ecp peers log --limit 20
ecp peers thread <msg-id>

# Cleanup
ecp peers watch --stop && ecp peers gc
```

The `watcher` field distinguishes `alive` | `dead` | `not-started` — crashes don't masquerade as "feature not used."

### Provable code-review verdicts

`ecp review --verdicts` pre-computes graph-backed verdicts from `ecp diff` sections. Pass the JSON directly as review context — skip LLM re-derivation of caller relationships from a raw diff.

```bash
ecp review --since main --verdicts --format json
```

| Severity | Rule |
|---|---|
| `RISK` | Cross-file callers exist, public symbol removed, or blindspot in diff region |
| `WARN` | Intra-file callers only, or route modified |
| `INFO` | No callers found, or new public surface added |

Verdict kinds: `SIGNATURE_OR_BODY_CHANGED` · `NEW_PUBLIC_SURFACE` · `REMOVED_PUBLIC_SURFACE` · `ROUTE_CONTRACT_CHANGED` · `BLINDSPOT_IN_DIFF_REGION`

Every verdict cites the exact diff section and graph fact that triggered it. Full spec: [docs/specs/2026-05-22-review-verdicts.md](./docs/specs/2026-05-22-review-verdicts.md).

---

## 🔌 Agent integration

**Prefer the native path** where available — it wires auto-reindex hooks and workflow skills that teach the agent *when* graph queries are worth the round-trip. **MCP is the universal fallback** for any host that speaks the protocol.

| Agent | Path | Wires |
|---|---|---|
| Claude Code | native | hooks + skills + optional MCP |
| Codex CLI | native | skills (native-tools pending) |
| Gemini CLI | native | native skill **or** MCP |
| Cursor · Windsurf · Cline · Copilot · any MCP host | MCP | MCP server |

Guided setup: `ecp admin → Agent Integrations → <host>`. Scriptable path for automation: `ecp admin <host> install <component>`. Inspect any host: `ecp admin <host> status`.

### Claude Code

```bash
ecp admin claude install hooks          # settings.json: auto-reindex + context enrichment
ecp admin claude install skills all     # ecp + simplify skill packs (or: ecp | simplify)
ecp admin claude install mcp-server     # optional — hooks + skills + CLI already sufficient
```

Hooks feed graph context on every Grep/Glob/Bash without an explicit tool call. The `ecp` skill teaches symbol / impact / route / contract / rename workflows. `simplify` drives graph-first code review.

### Gemini CLI

```bash
ecp admin gemini install native-skill   # links via `gemini skills link`
ecp admin gemini install mcp-server     # registers via `gemini mcp add`
```

`native-skill` and `mcp-server` are mutually exclusive — installing one removes the other.

### Codex CLI

```bash
ecp admin codex install skills all      # ecp + simplify; native-tools pending Codex wiring
```

**Workflow skills:**

| Skill | Use when |
|---|---|
| `ecp` | Agent decides whether graph-aware workflows beat grep / file reads for symbols, callers, routes, contracts |
| `simplify` | Code review starting from ecp impact, blind spots, egress, shape drift, resolver deltas |

### MCP fallback (Cursor, Windsurf, Cline, any MCP host)

| Host | Config file |
|---|---|
| Cursor | `~/.cursor/mcp.json` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` |
| Cline (VS Code) | `cline_mcp_settings.json` (MCP panel → "Edit MCP Settings") |
| Generic MCP host | host-specific |

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

```bash
ecp admin mcp tools    # verify exposed surface before connecting
ecp admin mcp serve    # stateless one-shot per call (no warm-up cost)
```

---

## 🏗️ Architecture

```
crates/
├── ecp-core        # Zero-copy graph (rkyv + mmap), incremental cache, graph queries
├── ecp-analyzer    # Tree-sitter parsers, HTTP route detector, framework confidence
├── ecp-mcp         # MCP server (stdio) — exposes core commands as tools
└── ecp-cli         # `ecp` binary, Tantivy BM25 engine, token-optimized output
```

Parse → resolve → serialize runs through an MPSC channel into a single builder thread that assembles the graph and writes a zero-copy `.ecp/graph.bin`. Read paths (`inspect`, `cypher`, `impact`, …) mmap this file directly — no deserialization step. xxh3_64 content cache keeps incremental rebuilds sub-second on a 22k-file repo.

---

## 🌐 Language coverage

31 languages parsed at the structural level. **14 full-depth** (TypeScript, JavaScript, Python, Java, Kotlin, C#, Go, Rust, PHP, Ruby, Swift, C, C++, Dart) — imports, named bindings, exports, heritage, types, constructors, config, frameworks, entry points, calls, and rename. **17 structural-only**: Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig.

📊 **[Full Language Capability Matrix](./docs/language-matrix.md)** — per-language status and rationale.

---

## ⚙️ Tuning

| Env var | Default | Effect |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216` (16 MiB) | Skip source files above this size during ingest. Caps worst-case worker RAM at `num_threads × MAX`. |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | `*.csproj` discovery recursion depth. Raise for deeply-nested .NET monorepos. |

---

## 📜 License & acknowledgments

[PolyForm Noncommercial 1.0.0](./LICENSE.md). Personal use, research, hobby projects, and noncommercial organizations explicitly permitted. **Commercial use is not granted by this license** — contact the upstream GitNexus author Abhigyan Patwari for commercial rights.

Built on:
- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — original design, CLI surface, and conceptual model
- [tree-sitter](https://tree-sitter.github.io/) — robust incremental AST parsing
- [rkyv](https://rkyv.org/) — zero-copy deserialization framework
- [Tantivy](https://github.com/quickwit-oss/tantivy) — full-text search engine
- [Rayon](https://github.com/rayon-rs/rayon) — data parallelism for multi-core concurrent AST parsing
- [xxhash (xxh3_64)](https://xxhash.com/) — non-cryptographic hashing for content-based incremental indexing
- [DashMap](https://github.com/xacrimon/dashmap) — concurrent hash maps for graph assembly
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — zero-copy memory mapping for sub-millisecond graph access
- [msgspec](https://github.com/jcrist/msgspec) — high-performance JSON serialization for inter-process communication

Agent onboarding (URL bootstrap, Claude Code skill, plugin install): `docs/skills/ecp-onboard/`. Concurrency invariants and re-verification: `./scripts/audit/audit-concurrency.sh`.

## 🚦 Release status

Verified install path: `cargo install --git ...`, which builds `ecp` from source. Release installers already contain the checksum and provenance-verification flow, but require a published tag and release assets before the binary download path is end-to-end verified. Agent-facing onboarding skill: [docs/skills/ecp-onboard/ONBOARDING.md](./docs/skills/ecp-onboard/ONBOARDING.md). Assisted configuration/setup flow still being refined.

---

<div align="center">

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)

</div>
