# EgentCodePlexus

Structural code knowledge for AI agents. One-shot CLI, zero-copy mmap, ~140 ms per query.

`cold index 2.60 s · query p50 142 ms · 31 languages · BlindSpot edges (no hallucinated dispatch) · 60× upstream gitnexus`

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)
[![Linux](https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black)](https://github.com/coseto6125/egent-code-plexus/releases)
[![macOS](https://img.shields.io/badge/macOS-000000?style=for-the-badge&logo=apple&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/releases)
[![Windows](https://img.shields.io/badge/Windows-0078D6?style=for-the-badge&logo=windows&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/releases)
[![Claude Code](https://img.shields.io/badge/Claude_Code-D97757?style=for-the-badge&logo=anthropic&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/skill_sample/claude/SKILL.md)
[![Codex CLI](https://img.shields.io/badge/Codex_CLI-412991?style=for-the-badge&logo=openai&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/skill_sample/codex/ecp/SKILL.md)
[![Cursor](https://img.shields.io/badge/Cursor-000000?style=for-the-badge&logo=cursor&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/docs/skills/ecp-onboard/guides/04-mcp.md)

**English** · [繁體中文](./docs/readme_i18n/README_zh-TW.md) · [简体中文](./docs/readme_i18n/README_zh-CN.md) · [Español](./docs/readme_i18n/README_es.md) · [Русский](./docs/readme_i18n/README_ru.md) · [हिन्दी](./docs/readme_i18n/README_hi.md) · [日本語](./docs/readme_i18n/README_ja.md) · [한국어](./docs/readme_i18n/README_ko.md) · [Português (BR)](./docs/readme_i18n/README_pt-BR.md)

---

## The case

Code agents fan out 20–50 lookups per task. Grep returns strings; an autonomous agent needs symbols, callers, edges, and an honest signal when the static graph runs out.

`ecp` is the structural-knowledge layer that is:

- **stateless.** Every invocation `mmap`s a zero-copy `rkyv` graph and exits. No daemon to keep warm, no "server died, restart" failure mode.
- **honest.** Unresolvable call sites (dynamic dispatch, unresolved import, reflection) become `BlindSpot` records. An agent that acts on a hallucinated dependency costs more than one that gets an "I don't know" and routes around.
- **token-cheap.** Default output is TOON (compact key:value). Every flag surfaces via `--help`. Every command is non-interactive and stdout-parseable.
- **polyglot.** 31 languages parsed at the structural level — service code, Dockerfiles, GitHub Actions, Terraform, SQL, and smart contracts stay legible the moment you leave the main language.

Built on top of [GitNexus](https://github.com/abhigyanpatwari/GitNexus) by [Abhigyan Patwari](https://github.com/abhigyanpatwari) — same conceptual model, rewritten in Rust for a different audience. 🎙️ [Agent interviews](./interviews/README.md) — Gemini CLI and Codex evaluate `ecp` in autonomous workflows.

## Receipts

Head-to-head vs. upstream GitNexus, measured on the [gitnexus](https://github.com/abhigyanpatwari/GitNexus) codebase (TypeScript) using `scripts/parity/benchmark_vs_gitnexus.py`:

| Phase | ecp (Rust) | gitnexus (Node) | Speedup |
|---|---|---|---|
| Cold Index | **~970 ms** | ~58 s | **60×** |
| Symbol Context | **~70 ms** | ~430 ms | **6×** |
| Blast Radius | **~70 ms** | ~460 ms | **6×** |
| Cypher Query | **~70 ms** | ~400 ms | **5×** |

`ecp` numbers include full process startup (no daemon). GitNexus (v1.6.5) numbers are against a warm, indexed repo via its CLI.

<details>
<summary><b>Scalability — single run on <code>.sample_repo</code></b> (2.1 GB polyglot, ~40 OSS projects, 25+ languages)</summary>

**Ingest performance**

| Phase | Value |
|---|---|
| Files indexed | **22,645** across 25 detected languages |
| Wall-clock (Cold) | **2.60 s** (parse + resolve + serialize) |
| Wall-clock (Incremental) | **4.9 ms** (xxh3_64 hash walk, zero dirty files) |
| Hardware | AMD Ryzen 9 9950X (16 logical), 39.2 GiB RAM, Linux 6.6.87 |

**Per-query latency** (including process startup)

| Query | Median | Notes |
|---|---|---|
| `summary` (registry overview) | **1.4 ms** | smallest read — just registry mmap |
| `routes` (HTTP route map across repo) | **142.3 ms** | enumerates declarative + imperative |
| `summary --detailed` (frameworks + blind-spots) | **143.4 ms** | full registry + per-framework scoring |
| `impact <symbol> --direction down` | **145.0 ms** | BFS over Calls / Extends edges |
| `inspect <symbol>` | **145.6 ms** | symbol resolution + 1-hop traversal |
| `find <name> --mode bm25` | **154.5 ms** | Tantivy query + 5-bucket partition |
| `cypher 'MATCH (a:Class)-[:HasMethod]->(b:Method) ...'` | **161.5 ms** | one pattern, one row |
| `cypher 'MATCH (a:Method)-[:Calls]->(b:Method) ...'` | **174.2 ms** | broader pattern, more matches |
| `impact --baseline HEAD~1` (change-set blast radius) | **359.0 ms** | git diff + parallel per-file parse + BFS |

Reproduce: `python scripts/benchmark/benchmark_ecp.py`.

</details>

## vs. upstream gitnexus

Same conceptual model, different audience. `ecp` is **not** a drop-in replacement — choose based on who reads the graph.

| Dimension | EgentCodePlexus | GitNexus |
|---|---|---|
| Primary consumer | Autonomous AI code agents | Human devs + IDE integration |
| Runtime | Stateless one-shot CLI (zero warm-up) | Long-running MCP server |
| Performance | **< 2.5 s cold index / < 150 ms query** | ~60 s cold index / ~400 ms query |
| Unresolved edge | `BlindSpot` record (honest unknown) | Heuristic guess |
| Default output | TOON / compact JSON (token-cheap) | Wiki / UI rendering |
| Languages | 31 (14 deep + 17 structural) | 14 (deep, 9-dimension) |
| Storage | Rust + `rkyv` zero-copy mmap | Node.js + LadybugDB |

Full 8-dimension breakdown + decision matrix → [docs/vs-gitnexus.md](./docs/vs-gitnexus.md).

## 30-second demo

```bash
$ ecp impact parse_with_budget --direction upstream --format toon
```

```text
target          parse_with_budget
  kind          Function
  file          crates/ecp-analyzer/src/parse_budget.rs:28
risk_level      HIGH
direct_callers  22 across 22 files
  crates/ecp-analyzer/src/python/parser.rs:48      Method parse_file
  crates/ecp-analyzer/src/rust/parser.rs:142       Method parse_file
  crates/ecp-analyzer/src/typescript/parser.rs:73  Method parse_file
  crates/ecp-analyzer/src/go/parser.rs:69          Method parse_file
  ... (18 more language parsers)
transitive      231 symbols across language detection + pipeline
blind_spots     0
```

One process, one mmap, ~140 ms. Real symbol from this repo — every per-language `parser.rs` fans in via the budget primitive. Read-side commands accept `--format text|json|toon`; the default per command is whichever encoding is cheapest in tokens.

## Install

Prebuilt binaries ship with every GitHub Release. The installer scripts fall back to a cargo source build only when a matching release asset is unavailable.

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# Explicit cargo path (same source build, no installer wrapper)
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

<details>
<summary>CPU-tuned source build</summary>

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

</details>

## Quick start

```bash
# 1. Index the current repo (incremental; first query auto-indexes too)
ecp admin index --repo .

# 2. Locate a symbol — exact name by default
ecp find loginUser
ecp find login --mode bm25       # ranked BM25, top-K bucketed by source/tests/ref/doc/config

# 3. Blast radius — who breaks if I change this?
ecp impact validateUser --direction upstream

# 4. Full symbol context (signature, body, callers, callees, 1-hop impact)
ecp inspect validateUser

# 5. Every HTTP route in the repo (declarative @Get + imperative app.get())
ecp routes
ecp routes /api/users --method POST     # route → handler → caller chain

# 6. Path-literal site lookup — "who reads / writes this file?"
ecp impact --literal session_meta.json  # 14 langs; sink:read / sink:write / sink:join / sink:free
```

Read-side commands accept `--format text|json|toon`. Default per command is the token-cheapest representation (mostly `toon`; `find` defaults to `text`; `cypher` defaults to `json`).

## CLI surface

Two tiers — **agent commands** at top level (query / refactor / verify) and **admin commands** under `ecp admin` (registry / hooks / destructive). Run `ecp --help` and `ecp admin --help` for full flag matrices.

| Command | Purpose |
|---|---|
| `inspect <name>` | One symbol → metadata, decorators, signature, callers, callees, 1-hop impact, contained methods / properties / enum variants |
| `find <pattern>` | Locate symbols — exact (default) · `--mode fuzzy` substring · `--mode bm25` lexical ranking; bm25 partitions output into source / tests / reference / document / config buckets |
| `find-schema-bindings <field>` | Surface MirrorsField heuristic edges + blind-spot candidates (schema field mirrors across classes / services). |
| `find-transaction-patterns [--class <Name>]` | Detect Saga compensate/undo/rollback name-pairs on same class. Confidence ≥0.75 tier:POSSIBLY_RELATED, <0.75 tier:BLIND_SPOT. |
| `impact <name> --direction <up\|down>` | Blast-radius traversal with confidence filtering. `--baseline <ref>` for change-set impact (also `--literal <V>` for PathLiteral split-brain). |
| `rename --symbol <old> --new-name <new>` | AST-aware multi-file rename across 14 languages. Always `--dry-run` first. |
| `cypher '<query>'` | openCypher escape hatch; `m.content` returns source body. |
| `summary` | Registry overview, framework coverage, LLM-actionable blind-spot catalog, graph freshness. (Was `coverage`; the old verb still works as an alias.) |
| `routes [<path>]` | Enumerate HTTP routes (declarative + imperative); with `<path>` show handler + callers. |
| `contracts` | Cross-repo API contract inventory (routes / queue / RPC). |
| `diff` | Resolver-delta — edge-level binding tier-degradation + route / contract changes. |
| `tool-map` | Calls to external HTTP / DB / Redis / queue clients via per-file import-binding analysis. |
| `shape-check` | Drift between HTTP consumer access patterns and Route response shapes. |
| `peers` | Multi-session peer collaboration: `status` / `diff` / `say` / `inbox` / `log` / `thread` / `watch` / `gc`. |
| `review` | LLM-workflow audit aggregator — impact + summary + tool-map + shape-check + diff, filtered to high-confidence signals. |

<details>
<summary><b>Admin namespace</b> — <code>ecp admin &lt;cmd&gt;</code> (registry / hooks / destructive)</summary>

| Command | Purpose |
|---|---|
| `index --repo <path>` | Build / refresh the graph; incremental via xxh3_64 content cache. `--force` for full rebuild. |
| `drop / prune / rename-branch` | Index lifecycle: delete, prune stale branch dirs, rename branch on-disk. |
| `install-hook` | Install the git reference-transaction hook (auto-track branch switches). |
| `config` | Interactive TOML wizard for `.ecp/config.toml`. |
| `mcp serve` / `mcp tools` | MCP server (stdio) for LLM hosts; `tools` lists the exposed tool surface. |
| `claude install / codex install / gemini install` | Scriptable host integration (skills, hooks, MCP entries). |
| `verify-resolver` | Diff resolver dump against a language oracle (ecp-dev QA). |

</details>

All commands resolve `.ecp/graph.bin` from CWD unless `--graph <path>` is given. Agent-facing commands are non-interactive by design. Run `ecp admin` with no subcommand to open the interactive admin TUI.

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

## MCP server

`ecp` ships an MCP server exposing core commands as MCP tools. Hosts that speak MCP (Claude Code, Cursor, Windsurf, Cline, Codex CLI, Gemini CLI) register `ecp` and call the tools autonomously.

```bash
ecp admin mcp tools          # inspect what tools will be exposed
ecp admin mcp serve          # run the server
```

Scripted host install:

```bash
ecp admin claude install mcp-server
ecp admin gemini install skills
```

Progressive path (TUI): `ecp admin → Agent Integrations → MCP → <host> → install`.

<details>
<summary><b>Codex CLI native integration</b> (separate from MCP — prepares a patch for an openai/codex fork)</summary>

The Codex native path doesn't edit the running Codex installation; it writes a patch you apply to an `openai/codex` fork.

Progressive path: `ecp admin → Agent Integrations → Codex CLI → install → native-tools`.

Scripted:

```bash
ecp admin codex install native-tools
ecp admin codex install skills all       # or: ecp | simplify
```

Bundled skills teach workflow selection that command help can't infer:

| Skill | Use when |
|---|---|
| `ecp` | Agent needs to decide whether graph-aware symbol / impact / route / contract / rename workflows beat grep / file reads. |
| `simplify` | Agent is reviewing changed code and should start from `ecp impact`, blind spots, egress, shape drift, and resolver deltas before reading raw diffs. |

The `native-tools` component writes `~/.config/ecp/host-integration/codex-cli.patch`. Apply in your fork:

```bash
cd /path/to/openai-codex-fork
git apply ~/.config/ecp/host-integration/codex-cli.patch
```

Verify a fork that already has the native marker:

```bash
ECP_CODEX_CLI_CHECKOUT=/path/to/openai-codex-fork ecp admin codex status
ecp admin codex uninstall native-tools
ecp admin codex uninstall skills all
```

</details>

## Architecture

```
crates/
├── ecp-core        Zero-copy graph (rkyv + mmap), incremental cache, graph queries
├── ecp-analyzer    Tree-sitter parsers, HTTP route detector, framework confidence
├── ecp-mcp         MCP server (stdio) — exposes core commands as tools
└── ecp-cli         `ecp` binary, Tantivy BM25 engine, token-optimized output
```

Parse → resolve → serialize runs through an MPSC channel into a single builder thread that assembles the graph and writes a zero-copy `.ecp/graph.bin`. Read paths (`inspect`, `cypher`, `impact`, …) mmap this file directly. The xxh3_64 content cache keeps incremental rebuilds sub-second on a 22k-file repo.

## Language coverage

31 languages parsed at the structural level (functions / classes / methods / imports / calls). 14 of them — the original GitNexus set — get full-depth coverage across imports, named bindings, exports, heritage, types, constructors, config, frameworks, entry points, calls, and rename. The remaining 17 are structural-only (Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig).

📊 [Full Language Capability Matrix](./docs/language-matrix.md) — per-language status and rationale.

## Tuning

| Env var | Default | Effect |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216` (16 MiB) | Skip source files larger than this during ingest. Caps worst-case worker RAM at `num_threads × MAX`. |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | Directory recursion depth for `*.csproj` discovery. Raise for deeply-nested .NET monorepos. |

## License

Licensed under [PolyForm Noncommercial 1.0.0](./LICENSE.md). Personal use, research, hobby projects, and noncommercial organizations are permitted. **Commercial use is not granted by this license** — contact upstream GitNexus author [Abhigyan Patwari](https://github.com/abhigyanpatwari) for commercial rights. See [NOTICES.md](./LICENSES/NOTICES.md) for required attribution.

<details>
<summary><b>Built on</b> (acknowledgments)</summary>

- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — original design, CLI surface, conceptual model
- [tree-sitter](https://tree-sitter.github.io/) — incremental AST parsing
- [rkyv](https://rkyv.org/) — zero-copy deserialization framework
- [Tantivy](https://github.com/quickwit-oss/tantivy) — Rust BM25 search engine
- [Rayon](https://github.com/rayon-rs/rayon) — data parallelism for multi-core concurrent AST parsing
- [xxhash (xxh3_64)](https://xxhash.com/) — content-based incremental indexing
- [DashMap](https://github.com/xacrimon/dashmap) — concurrent hash maps for graph assembly
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — zero-copy memory mapping for sub-millisecond graph access
- [msgspec](https://github.com/jcrist/msgspec) — fast JSON serialization for IPC

Onboarding for AI agents lives at `docs/skills/ecp-onboard/`. Concurrency invariants and how to re-verify them: `./scripts/audit/audit-concurrency.sh`.

</details>

## Release status

The current verified install path is `cargo install --git ...`, which builds `ecp` from source. Release installers already contain the checksum and provenance-verification flow, but they require a published tag and release assets before the binary download path can be end-to-end verified. The agent-facing onboarding skill at [docs/skills/ecp-onboard/ONBOARDING.md](./docs/skills/ecp-onboard/ONBOARDING.md) guides users through install, first index, optional groups, MCP wiring, and next steps — the assisted setup flow is still being refined.

---

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)
