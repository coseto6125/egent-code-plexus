# Code Graph Nexus

A code intelligence graph for **LLMs and AI code agents** — one-shot CLI, zero-copy mmap, sub-second per query.

[繁體中文 (Traditional Chinese)](./README_zh-TW.md)

---

## 🎯 Mission

`cgn` exists to be the structural-knowledge layer that an autonomous AI coding agent calls 20–50 times per task. Every design decision falls out of that one premise:

- **Built for agents, not IDEs.** Output is token-cheap (TOON / compact JSON), every flag surfaces via `--help`, every command is non-interactive and stdout-parseable. No UI, no human-skim layout cruft eating the agent's context window.
- **No warm-up, no daemon.** Each invocation `mmap`s a zero-copy `rkyv` graph file and exits. Read queries return in **~140–170 ms** *including process startup*; a 22k-file repo cold-indexes in under 3 s. An agent can fire dozens of queries per task without amortising a server boot, and there is no "daemon died, please restart" failure mode.
- **Honest answers over readable graphs.** When a call site can't be statically resolved (dynamic dispatch, unresolved import, reflection), `cgn` emits a `BlindSpot` record — not a guessed edge. An agent that acts on a hallucinated dependency is much more expensive than one that gets an "I don't know" it can route around.
- **Polyglot reach.** 31 languages parsed at the structural level so modern multi-stack repos (service code + Dockerfiles + GitHub Actions + Terraform + SQL + smart contracts) stop being black holes the moment you leave the main language.

🎙️ **[Agent Interviews](./interviews/README.md)** — See how real AI agents (Gemini CLI, Codex) use and evaluate `cgn` in autonomous workflows.

Built on top of [GitNexus](https://github.com/abhigyanpatwari/GitNexus) by [Abhigyan Patwari](https://github.com/abhigyanpatwari) — same conceptual model (a structural knowledge graph of a repo), rewritten in Rust for a different audience. Licensed under [PolyForm Noncommercial 1.0.0](./LICENSE); see [NOTICES.md](./NOTICES.md) for required attribution.

---

## ⚡ Performance

The Mission section above is *why* `cgn` is built the way it is. This section is the receipts.

### Head-to-head vs. upstream GitNexus

Measured on the [gitnexus](https://github.com/abhigyanpatwari/GitNexus) codebase (TypeScript) using `scripts/parity/benchmark_vs_gitnexus.py`:

| Phase | cgn (Rust) | gitnexus (Node) | Speedup |
|---|---|---|---|
| **Cold Index** | **~970 ms** | ~58 s | **60×** |
| **Symbol Context** | **~70 ms** | ~430 ms | **6×** |
| **Blast Radius** | **~70 ms** | ~460 ms | **6×** |
| **Cypher Query** | **~70 ms** | ~400 ms | **5×** |

*Note: `cgn` query latency includes full process startup (no daemon). GitNexus (v1.6.5) query latency is against a warm, indexed repo via its CLI.*

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
| `coverage` (registry overview) | **1.4 ms** | smallest read — just registry mmap |
| `routes` (HTTP route map across repo) | **142.3 ms** | enumerates declarative + imperative |
| `coverage --detailed` (frameworks + blind-spots) | **143.4 ms** | full registry + per-framework scoring |
| `impact <symbol> --direction down` | **145.0 ms** | BFS over Calls / Extends edges |
| `inspect <symbol>` (signature + callers + callees) | **145.6 ms** | symbol resolution + 1-hop traversal |
| `find <name> --mode bm25` (lexical search) | **154.5 ms** | Tantivy query + 5-bucket partition |
| `cypher 'MATCH (a:Class)-[:HasMethod]->(b:Method) ...'` | **161.5 ms** | one pattern, one row returned |
| `cypher 'MATCH (a:Method)-[:Calls]->(b:Method) ...'` | **174.2 ms** | broader pattern, more matches |
| `impact --baseline HEAD~1` (change-set blast radius) | **359.0 ms** | git diff + parallel per-file parse + BFS |

Reproduce: `python scripts/benchmark_cgn.py`.

---

## vs. upstream GitNexus

Same conceptual model, different audience. `cgn` is **not** a drop-in replacement — choose based on who reads the graph and what they do with it.

| Dimension | Code Graph Nexus | GitNexus |
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

`cargo install --git` always works. Prebuilt binaries land per-platform once a tagged Release is published; the installer scripts auto-fall back to the cargo path until then.

```bash
# Cross-platform (needs rustup — first build is a few minutes, cached after)
cargo install --git https://github.com/coseto6125/code-graph-nexus code-graph-nexus --bin cgn --locked

# Linux / macOS one-liner (Release-first, cargo fallback)
curl -sSfL https://raw.githubusercontent.com/coseto6125/code-graph-nexus/main/install.sh | sh

# Windows PowerShell
iwr https://raw.githubusercontent.com/coseto6125/code-graph-nexus/main/install.ps1 -UseBasicParsing | iex
```

Self-install tuned for your CPU (fat LTO + native ISA):

```bash
RUSTFLAGS="-C target-cpu=native" \
  cargo install --git https://github.com/coseto6125/code-graph-nexus code-graph-nexus \
  --bin cgn --locked --profile release-dist
```

---

## 🚀 Quick start

```bash
# 1. Index the current repo (incremental; first query also auto-indexes)
cgn admin index --repo .

# 2. Locate a symbol — exact name by default
cgn find loginUser
cgn find login --mode bm25       # ranked BM25, top-K partitioned by source/tests/ref/doc/config

# 3. Blast radius — who breaks if I change this?
cgn impact validateUser --direction upstream

# 4. Full symbol context (signature, body, callers, callees, 1-hop impact)
cgn inspect validateUser

# 5. Every HTTP route in the repo (declarative @Get + imperative app.get())
cgn routes
cgn routes /api/users --method POST     # route → handler → caller chain
```

Read-side commands accept `--format text|json|toon`. Default per command is the token-cheapest representation (mostly `toon`; `find` defaults to `text`; `cypher`/`coverage` default to `json`).

---

## CLI surface

Two tiers — **agent commands** at top level (query/refactor/verify) and **admin commands** under `cgn admin` (registry/hooks/destructive). Run `cgn --help` and `cgn admin --help` for full flag matrices.

| Command | Purpose |
|---|---|
| `inspect <name>` | One symbol → metadata, decorators, signature, callers, callees, 1-hop impact |
| `find <pattern>` | Locate symbols — exact (default) · `--mode fuzzy` substring · `--mode bm25` lexical ranking; bm25 partitions output into source / tests / reference / document / config buckets |
| `impact <name> --direction <up\|down>` | Blast-radius traversal with confidence filtering. `--since <ref>` for change-set impact. |
| `rename --symbol <old> --new-name <new>` | AST-aware multi-file rename across 14 languages. Always `--dry-run` first. |
| `cypher '<query>'` | openCypher escape hatch; `m.content` returns source body. |
| `coverage` | Registry overview, framework coverage, blind-spot catalog, graph freshness. |
| `routes [<path>]` | Enumerate HTTP routes (declarative + imperative); with `<path>` show handler + callers. |
| `contracts` | Cross-repo API contract inventory (routes / queue / RPC). |
| `diff` | Resolver-delta — edge-level binding tier-degradation + route / contract changes. |
| `tool-map` | Calls to external HTTP / DB / Redis / queue clients via per-file import-binding analysis. |
| `shape-check` | Drift between HTTP consumer access patterns and Route response shapes. |
| `peers` | Multi-session peer collaboration (status / diff / log / gc). |
| `review` | Aggregated LLM-workflow audit: runs impact + coverage + tool-map + shape-check + diff in one shot, filtered to high-confidence signals. |

Admin namespace (`cgn admin <cmd>` — hidden from top-level help):

| Command | Purpose |
|---|---|
| `index --repo <path>` | Build / refresh the graph; incremental via xxh3_64 content cache. `--force` for full rebuild. |
| `drop / prune / rename-branch` | Index lifecycle: delete, prune stale branch dirs, rename branch on-disk. |
| `install-hook` | Install the git reference-transaction hook (auto-track branch switches). |
| `config` | Interactive TOML wizard for `.cgn/config.toml`. |
| `mcp serve` / `mcp tools` | MCP server (stdio) for LLM hosts; `tools` lists the exposed tool surface. |

All commands resolve `.cgn/graph.bin` from CWD unless `--graph <path>` is given. The CLI is non-interactive by design — every flag surfaces via `--help`, every output stream is parseable.

---

## MCP server (for LLM hosts)

`cgn` ships an MCP server exposing core commands as MCP tools. Hosts that speak MCP (Claude Code, Cursor, Windsurf, Cline, Codex CLI, Gemini CLI) can register `cgn` and call the tools autonomously.

```bash
cgn admin mcp tools          # inspect what tools will be exposed
cgn admin mcp serve          # run the server (default: spawn mode, fresh subprocess per call)
```

Manual host config example for Claude Code (`~/.config/claude-code/mcp-servers.json`):

```json
{
  "mcpServers": {
    "cgn": { "command": "cgn", "args": ["admin", "mcp", "serve"] }
  }
}
```

A `cgn admin` TUI for one-command installation across multiple hosts ships in a follow-up release.

---

## Architecture

```
crates/
├── cgn-core        # Zero-copy graph (rkyv + mmap), incremental cache, graph queries
├── cgn-analyzer    # Tree-sitter parsers, HTTP route detector, framework confidence
├── cgn-mcp         # MCP server (stdio) — exposes core commands as tools
└── cgn-cli         # `cgn` binary, Tantivy BM25 engine, token-optimized output
```

Parse → resolve → serialize runs through an MPSC channel into a single builder thread that assembles the graph and writes a zero-copy `.cgn/graph.bin`. Read paths (`inspect`, `cypher`, `impact`, …) mmap this file directly. The xxh3_64 content cache keeps incremental rebuilds at sub-second on a 22k-file repo.

---

## Language coverage

31 languages parsed at the structural level (functions / classes / methods / imports / calls). 14 of them — the original GitNexus set — get full-depth coverage across imports, named bindings, exports, heritage, types, constructors, config, frameworks, entry points, calls, and rename. The remaining 17 are structural-only (Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig).

📊 **[Full Language Capability Matrix](./docs/language-matrix.md)** — Detailed per-language status and rationale.

---

## ⚙️ Tuning

| Env var | Default | Effect |
|---|---|---|
| `CGN_MAX_FILE_BYTES` | `16777216` (16 MiB) | Skip source files larger than this during ingest. Caps worst-case worker RAM at `num_threads × MAX`. |
| `CGN_CSPROJ_MAX_DEPTH` | `4` | Directory recursion depth for `*.csproj` discovery. Raise for deeply-nested .NET monorepos. |

---

## License & acknowledgments

Licensed under [PolyForm Noncommercial 1.0.0](./LICENSE). Personal use, research, hobby projects, and noncommercial organizations are explicitly permitted. **Commercial use is not granted by this license** — contact the upstream GitNexus author Abhigyan Patwari for commercial rights.

Built on:
- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — original design, CLI surface, and conceptual model
- [tree-sitter](https://tree-sitter.github.io/) — robust incremental AST parsing
- [rkyv](https://rkyv.org/) — zero-copy deserialization framework
- [Tantivy](https://github.com/quickwit-oss/tantivy) — blazing fast Rust full-text search engine
- **Rayon** — data parallelism for multi-core concurrent AST parsing
- **xxhash (xxh3_64)** — extremely fast non-cryptographic hashing for content-based incremental indexing
- **DashMap** — high-performance concurrent hash maps for graph assembly
- **memmap2** — zero-copy memory mapping for sub-millisecond graph access
- **msgspec** — high-performance JSON serialization for inter-process communication

Onboarding for AI agents (URL bootstrap, Claude Code skill, plugin install) lives at `docs/skills/cgn-onboard/`. Concurrency invariants and how to re-verify them: `./scripts/audit-concurrency.sh`.
