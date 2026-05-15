# Graph Nexus for LLM

A code intelligence graph built for **LLMs and AI code agents** — not for human IDE integration. Indexes any codebase in milliseconds, then answers structural questions like *who calls this*, *what's the blast radius if I change this function*, or *what's related to the auth flow*.

Built on top of [GitNexus](https://github.com/abhigyanpatwari/GitNexus) by [Abhigyan Patwari](https://github.com/abhigyanpatwari) — same core idea (a structural knowledge graph of a repo), rewritten in Rust for a different audience. Licensed under [PolyForm Noncommercial 1.0.0](./LICENSE).

> Required Notice: Copyright Abhigyan Patwari (https://github.com/abhigyanpatwari/GitNexus). Not affiliated with or endorsed by the upstream GitNexus project. Noncommercial use only. See [NOTICES.md](./NOTICES.md) for the full third-party attribution list.

## vs. upstream GitNexus

> **Not a drop-in replacement.** Upstream is a broader Node/TypeScript agent platform (MCP server, resources, hooks, generated skills); graph-nexus is a stateless Rust CLI optimized for shell-mediated LLM calls — different scope, different tradeoffs.

| Dimension | GitNexus | graph-nexus | Why it matters for an LLM agent |
|---|---|---|---|
| **Audience** | Human devs + IDE integration | AI code agents | Optimisation target shapes every other row |
| **Runtime** | Long-running MCP server | One-shot CLI, rkyv mmap zero-copy | Sub-second per query; an agent can fire 30+ queries per task with no warm-up cost |
| **Unresolved import** | Heuristic guess (e.g. Jaccard) to keep the graph readable | `BlindSpot` record, no edge — never fabricate | Agent never acts on a hallucinated dependency; an honest "I don't know" beats a confident wrong answer |
| **Output format** | Wiki / UI rendering | `etoon` / `cypher` / compact JSON | No UI cruft eating context window; tokens spent on graph, not on layout |
| **Languages parsed** | 14 (TypeScript, JavaScript, Python, Java, Kotlin, C#, Go, Rust, PHP, Ruby, Swift, C, C++, Dart) | 31 — same 14 plus Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig | Mixed-stack repos (DevOps configs, Web3 contracts, infra-as-code) stop being black holes |

> Language depth varies. graph-nexus parses 31 languages at the structural level (functions / classes / methods / imports); it does not yet match GitNexus's full 9-dimension coverage (Named Bindings, Heritage, Constructor Inference, Config, ...) on every language. Treat the 31 count as breadth, not parity — see [Language Matrix](#language-matrix) below.

### Tool & integration coverage

| LLM-facing area | Upstream GitNexus (`._source_code`) | Graph Nexus Rust (`gnx`) |
| :--- | :--- | :--- |
| **Agent integration** | MCP server, resources, prompts, setup, hooks, generated skills | Stateless CLI; use through shell/tool wrappers. **No built-in MCP server yet.** |
| **Core query tools** | `query`, `context`, `impact`, `detect_changes`, `rename`, `cypher`, group tools | `query`, `context`, `impact`, `detect-changes`, `route-map`, `cypher`, `summarize`, `rename` |
| **Context output** | Rich MCP responses and generated repo skills | Compact `toon`/JSON/text for shell-mediated LLM calls |
| **Search** | Documented BM25 + semantic + RRF hybrid search | Embeddings when available; otherwise Tantivy BM25 fallback |
| **Runtime/storage** | Node.js + LadybugDB | Rust + mmap `rkyv` graph file |
| **Best fit** | Agent runtimes with strong MCP/editor integration | Local LLM harnesses/scripts that want a small executable with few moving parts |

Under the hood: zero-copy on-disk storage (rkyv + mmap), hybrid search (BM25 via Tantivy + BGE-M3 dense vectors), framework-aware route extraction. The CLI is `gnx`.

[繁體中文說明 (Traditional Chinese)](./README_zh-TW.md)

## 🚀 Key Features

*   **Blazing Fast & Zero-Copy**: cold-indexed `.sample_repo` — **22,772 files across 25 detected languages in 4.9 s** (Java 3535, PHP 2907, TypeScript 1704, C# 945, Rust 870, C 801, Markdown 783, Dart 616, Bash 487, C++ 476, JavaScript 466, Solidity 403, Move 367, YAML 343, Ruby 156, Python 134, Swift 105, Go 99, Crystal 72, Kotlin 49, Lua 32, Zig 31, Dockerfile 20, Docker Compose 8, SQL 4). Per-query latency on the same graph: cypher 9 ms · context 9 ms · impact 5–6 ms · route-map 13 ms · BM25 query 24 ms · summarize 38 ms · detect-changes 230 ms. Hardware: **AMD Ryzen 9 9950X (8 vCPU under WSL2, 11.7 GiB RAM)**, Linux 6.6.87. Tree-sitter + Rayon for parse, `rkyv` mmap for zero-copy `graph.bin`. Reproduce: `python scripts/benchmark_gnx.py`.
*   **LLM-Native Output**: Emits extreme token-efficient formats ([TOON](https://crates.io/crates/etoon)) and concise string summaries. No hallucination-inducing formatting.
*   **Hybrid Search Engine**:
    *   **Semantic Search**: Uses **BGE-M3 INT8 Quantized Model** via `fastembed-rs` (`--embeddings`). Cross-lingual concept matching (e.g., search "Session Management" in Chinese, find English functions) with AVX2 CPU acceleration and massive memory reduction.
    *   **Lexical Search**: Uses **Tantivy (BM25)** for zero-latency, full-text tokenized keyword matching.
*   **Incremental Caching**: Only re-computes ASTs and Embeddings for modified files (SHA-256 Content Hash). Graph rebuilds drop from ~50s (Cold Start) to **< 0.25s**!
*   **Zero-Maintenance Route Extraction**: Purely based on RFC 7231 HTTP constants. Extracts API routes from both Declarative (e.g., `@Get`) and Imperative (e.g., `app.get()`) definitions across all languages.
*   **RAG Document Indexing**: Securely isolates `.md` (Markdown) and `.yaml` (GitHub Actions) files into parallel structures, parsing sections natively for LLM documentation retrieval without polluting the code execution graph.

## 📦 Installation

| Platform / user | Command | Notes |
| :--- | :--- | :--- |
| macOS Homebrew | `brew tap coseto6125/tap && brew install graph-nexus` | Use after the tap formula is published. Package: `graph-nexus`; binary: `gnx` |
| Linux / macOS | `curl -sSfL https://github.com/coseto6125/graph-nexus/releases/latest/download/install.sh \| sh` | Installs the prebuilt GitHub Release binary |
| Windows PowerShell | `irm https://github.com/coseto6125/graph-nexus/releases/latest/download/install.ps1 \| iex` | Installs the prebuilt GitHub Release binary |
| Rust source build | `cargo install --git https://github.com/coseto6125/graph-nexus --bin gnx` | Works before crates.io publishing |
| Manual | Download from [GitHub Releases](https://github.com/coseto6125/graph-nexus/releases) | Pick the archive for your target and verify `.sha256` |

> `cargo install graph-nexus` is intentionally not listed yet: crates.io publish is blocked until all analyzer grammar dependencies are available as publishable crate dependencies.

After install, the binary is named `gnx` (the package on crates.io is `graph-nexus`).

## ⚡ Usage

### Quick start

```bash
# 1. Build a code graph for the current repo (Extremely fast, < 1s)
gnx analyze --repo .

# 2. Build with BGE-M3 Semantic Embeddings (Downloads ~540MB INT8 model on first run)
gnx analyze --repo . --embeddings

# 3. Hybrid Search: Semantic Concept (Requires --embeddings)
gnx query --query "authentication flow"

# 4. Hybrid Search: Exact Keyword BM25 (Uses Tantivy)
gnx query --query "loginUser"

# 5. Extract all API Routes across the Microservice
gnx route-map --repo .

# 6. Find a symbol's blast-radius / execution flow
gnx impact --target validateUser --direction upstream

# 7. Explore Context (Metadata, Decorators, Signatures)
gnx context --name validateUser
```

Every read-side command accepts `--format text|json|toon`. The default is the token-cheapest representation per command (most: `toon`; `query`: `text`; `cypher`/`status`/`process`: `json`; `summarize`/`doctor`: `md`/`compact`).

### Task → command

| Goal | Use |
|---|---|
| Index a fresh repo | `gnx analyze --repo .` (or `gnx analyze-here` from inside the repo) |
| Re-index after edits | Same — `analyze` is incremental (SHA-256 content hash per file) |
| Symbol exists? Where? | `gnx query --query <name>` (BM25 + optional semantic) |
| One symbol → metadata, callers, callees | `gnx context --name <name>` |
| If I edit X, what breaks? | `gnx impact --target <name> --direction upstream` |
| What does X depend on? | `gnx impact --target <name> --direction downstream` |
| Arbitrary graph traversal / source body | `gnx cypher 'MATCH (m:Method) WHERE … RETURN m.content'` |
| List every HTTP route | `gnx route-map` |
| Who calls `POST /api/users`? | `gnx api-impact --route /api/users --method POST` |
| Where do we call external HTTP / DB / Redis / queue? | `gnx tool-map [--category http,db,redis,queue]` |
| Trace one execution flow start-to-finish | `gnx process --name <name>` |
| Architecture / hottest files / top symbols | `gnx summarize` |
| Coverage report (frameworks parsed, blind spots) | `gnx doctor` |
| What changed in this commit and what it ripples to | `gnx detect-changes --scope compare --base-ref HEAD~1` |
| Rename a symbol across files (currently Python MVP) | `gnx rename --symbol old --new-name new --dry-run` then drop `--dry-run` |
| List repos this machine has indexed | `gnx list` |
| Re-register a `.gitnexus-rs/` folder after moving the repo | `gnx index <path>` |
| Drop an index entirely | `gnx clean --repo <path>` |
| Multi-branch / multi-worktree workflows | `gnx init` (install hook), `gnx prune --branch X`, `gnx rename-branch --from A --to B` |
| Interactive setup wizard | `gnx config` |
| Check if the on-disk graph is stale | `gnx status` |
| List members of a graph community/cluster | `gnx cluster --id <n>` or `--name <anchor>` |
| Verify resolver decisions vs a language oracle | `gnx verify-resolver --oracle … --gnx … --lang <ts\|py\|rs>` |

### Command reference

All commands resolve `.gitnexus-rs/graph.bin` from the current dir unless `--graph <path>` is given. Read-only commands take `--repo <name-or-path>` to disambiguate when multiple repos are registered.

#### Index lifecycle

| Command | Purpose | Key flags |
|---|---|---|
| `analyze --repo <path>` | Build / refresh the graph for `<path>`. Incremental by default (content-hash cache). | `--embeddings` (build BGE-M3 vectors) · `--drop-embeddings` · `--force` (full rebuild) · `--dump-resolver <file>` |
| `analyze-here` | Convenience wrapper for `analyze --repo .`. | Same flags + `--no-cache` |
| `init` | Install the git reference-transaction hook so branch switches auto-track. | `--force` · `--no-chain` |
| `prune --branch <name> --repo <p>` | Drop a stale branch-scoped index dir. | — |
| `rename-branch --from <a> --to <b> --repo <p>` | Rename a branch's on-disk index. | — |
| `clean [--repo <p>] [--all]` | Delete the `.gitnexus-rs/` for a repo (or all). | — |
| `index [<path>]` | Re-register an existing `.gitnexus-rs/` after the repo moves. | — |
| `remove <target>` | Drop a registry entry by name / alias / path. | `--force` (reserved) |
| `list` | List every repo this machine has indexed. | `--format text\|json\|toon` |
| `status` | Per-repo staleness check (graph vs working tree). | `--repo <p>` |
| `config` | Interactive TOML wizard for `.gitnexus-rs/config.toml`. | `--repo <p>` |

#### Query the graph

| Command | Purpose | Key flags |
|---|---|---|
| `query --query <text>` | BM25 (+ optional semantic) symbol search by name / concept. | `--format` |
| `context --name <sym>` / `--uid <UID>` | One symbol → metadata, decorators, signature, callers, callees. | `--kind` · `--file_path` · `--relation_types` · `--include_tests` |
| `impact --target <sym> --direction <dir>` | Blast radius / dependency traversal. `dir` ∈ `upstream` (who calls X), `downstream` (what X calls). | `--depth <n>` (default 5) · `--high-trust-only` · `--min-confidence <f>` · `--include-tests` · `--kind` · `--file_path` |
| `cypher '<query>'` | Arbitrary openCypher pattern matching. `m.content` returns source body. | `--format` |
| `process --name <name>` | Per-process step trace (call chain for one execution flow). | — |
| `cluster --id <n>` / `--name <anchor>` | List members of a graph community / cluster. | — |

#### HTTP routes & tool calls

| Command | Purpose | Key flags |
|---|---|---|
| `route-map` | Enumerate every HTTP route the analyzer extracted (declarative `@Get` and imperative `app.get()`). | — |
| `api-impact --route <path>` | A route → its handler → upstream callers. | `--method GET\|POST\|…` · `--depth <n>` (default 3) |
| `tool-map` | Calls to known HTTP / DB / Redis / queue clients. | `--category http,db,redis,queue` |

#### Insights & change tracking

| Command | Purpose | Key flags |
|---|---|---|
| `summarize` | Markdown / JSON project overview: architecture, top files (in-edge centrality), top symbols. | `--top-files <n>` · `--top-communities <n>` · `--top-symbols <n>` · `--include-orphans` · `--output <file>` |
| `doctor` | Framework coverage + blind-spot catalog + graph status. The "LLM contract" report. | `--format compact\|json` |
| `detect-changes` | Symbols changed by git diff + affected execution flows. | `--scope unstaged\|staged\|all\|compare` · `--base-ref <ref>` (required with `compare`) · `--kind` · `--include-tests` · `--high-trust-only` |

#### Refactoring

| Command | Purpose | Key flags |
|---|---|---|
| `rename --symbol <old> --new-name <new>` | AST-powered multi-file rename (Python MVP). Always run `--dry-run` first. | `--dry-run` |

#### Diagnostics

| Command | Purpose | Key flags |
|---|---|---|
| `verify-resolver --oracle <f> --gnx <f> --lang <ts\|py\|rs>` | Diff a resolver-decision dump against a language oracle. Used by the parity harness. | `--report <md-path>` |

> Every command's flags can be re-confirmed with `gnx <command> --help`. The CLI is non-interactive by design (LLM-friendly): all flags surface via `--help`, all output goes to stdout in a parseable format.

## Language Matrix

For the 14 languages graph-nexus shares with upstream, here's the per-cell delta from an evidence-based audit. Each cell compares upstream GitNexus's claimed support against our actual implementation in `crates/graph-nexus-analyzer/src/<lang>/`.

**Legend**:
- ✓ &nbsp;both upstream and graph-nexus support this
- ✅ &nbsp;**upstream doesn't claim it, graph-nexus does** (where we go beyond upstream)
- ⚠️ &nbsp;**upstream claims it, graph-nexus is missing or partial** (where we lag)
- — &nbsp;neither claims it

| Language | Imports | Named | Exports | Heritage | Types | Ctor | Config | Frameworks | Entry | Call |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| TypeScript | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| JavaScript | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ |
| Python | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Java | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✅ | ✓ | ✓ | ✓ |
| Kotlin | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✅ | ✓ | ✓ | ✓ |
| C# | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ⚠️ | ✓ | ✓ |
| Go | ✓ | ✅ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Rust | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✅ | ✓ | ✓ | ✓ |
| PHP | ✓ | ✓ | ✓ | ✅ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Ruby | ✓ | — | ✓ | ✓ | — | ✓ | ✅ | ⚠️ | ✓ | ✓ |
| Swift | ✅ | — | ✓ | ✓ | ⚠️ | ✓ | ✓ | ⚠️ | ✓ | ✓ |
| C | ✅ | — | ✓ | ✅ | ✓ | ✓ | ✅ | ⚠️ | ✓ | ✓ |
| C++ | ✅ | ✅ | ✓ | ✓ | ✓ | ✓ | ✅ | ⚠️ | ✓ | ✓ |
| Dart | ✓ | ✅ | ✓ | ✓ | ⚠️ | ✓ | ✅ | ⚠️ | ✓ | ✓ |

**Where graph-nexus goes beyond upstream** (15 ✅ cells): C/C++ get Imports & Heritage that upstream doesn't claim; Java/Kotlin/Rust/Ruby/Dart get Config parsing for toolchains upstream doesn't cover; PHP gets Heritage; Go/C++/Dart get Named Bindings; Swift/C/C++ get basic Imports.

**Wave 1 closed 28 cells** (Constructor Inference rolled out to all 14 languages mirroring Python's `4e4fb1b` receiver-type binding prototype; Java static-import named bindings; C# `csproj`/`global.json` config; Exports for Go/Ruby/C/Dart per language conventions; cross-language Entry Point scorer combining routes + `main()` + framework decorators).

**Wave 2 (PR [#2](https://github.com/coseto6125/graph-nexus/pull/2))** closes 9 cells:
- **Types**: Go / C / C++ — declared types on params, returns, struct fields, vars.
- **Config**: PHP (`composer.json`) + Swift (`Package.swift`).
- **Frameworks**: JS (Express + Hapi), Kotlin (Ktor), Go (gin + echo), PHP (Laravel). Ported from upstream `gitnexus/src/core/group/extractors/http-patterns/` where the equivalent plugins exist.

**Remaining ⚠️ (8 cells)**: Frameworks for C# / Ruby / Swift / C / C++ / Dart — **upstream gitnexus has no plugin for any of these**, so the parity baseline is `—` rather than `⚠️`; what we add here is net-new beyond upstream. Types for Swift / Dart deferred — grammar shape varies enough that the SA dispatch in this wave didn't converge; queued for a focused follow-up.

**Matrix-opt batch (HEAD `86e65a7`)** deepened existing ✓ cells: Go gained per-struct-field visibility, Dart per-symbol underscore convention, Ruby `attr_*` metaprogramming + `include`/`extend` mixin tracking, TS/JS re-export alias preservation. See `docs/specs/2026-05-15-matrix-optimization-opportunities.md`.

### Extra languages (no upstream baseline)

Beyond the 14 main languages, the Rust providers also cover **17 additional languages**. Most are config / DSL / hardware — only structural extraction applies. Below is what each one actually carries today (✓ has dedicated capture, — not applicable, ⚠️ partial / heuristic):

| Language | Imports | Functions | Classes | Heritage | Calls | Routes | Config | Frameworks |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| Bash | ✓ (`source`/`.`) | ✓ | — | — | ✓ | — | — | — |
| Lua | ✓ (`require` + alias) | ✓ | ⚠️ (metatable heuristic) | ✓ (metatable `__index`) | ✓ | — | — | — |
| Solidity | ✓ | ✓ (incl. modifiers) | ✓ (contracts) | ✓ | ✓ | — | — | — |
| Crystal | ✓ | ✓ | ✓ | ✓ | ✓ | — | — | — |
| Nim | ✓ | ✓ | ✓ | ✓ | ✓ | — | — | — |
| Cairo | ✓ | ✓ | ✓ | — | ✓ | — | — | — |
| Move | ✓ | ✓ | ✓ (structs) | — | ✓ | — | — | — |
| Zig | ✓ | ✓ | ✓ (structs/unions) | — | ✓ | — | — | — |
| HCL | ✓ | ✓ (blocks) | — | — | ✓ | — | — | — |
| SQL | — | ✓ (procs/funcs) | ✓ (tables/views) | ⚠️ (FK refs) | ✓ | — | — | — |
| Verilog | ✓ | ✓ (modules) | — | — | ✓ | — | — | — |
| Vyper | ✓ | ✓ | ✓ (contracts) | — | ✓ | — | — | — |
| Markdown | — | — | — | — | — | — | — | — |
| GitHub Actions (`.yaml`) | ⚠️ (`uses:`) | ✓ (jobs/steps) | — | — | — | — | ✓ | — |
| Docker Compose | — | ✓ (services) | — | — | — | — | ✓ | — |
| Dockerfile | ✓ (`FROM` base) | — | — | — | — | — | ✓ | — |
| YAML (generic) | — | — | — | — | — | — | ✓ | — |

The matrix-opt batch added concrete dimensions to three of the extras: Bash `source`/`.` imports, Lua `local M = require()` aliases + metatable inheritance + table-assigned methods, Solidity state-variable visibility.

### Call detection design

Call detection is centralised in `crates/graph-nexus-analyzer/src/calls.rs`. The hot helper is `extract_calls(root, source, nodes, call_kinds)`:

- Each language parser passes the tree-sitter node kinds that represent a call in its grammar — e.g., `["call_expression"]` for JS/TS, `["function_call"]` for Lua, `["call"]` for Python.
- The walker is grammar-agnostic: descends the AST once, collects every call site, extracts the callee text via `callee_name_from(node, source)`, and attaches each call to its enclosing `Function` / `Method` via `attach_to_enclosing(line, callee, nodes)` (smallest-span containment).
- OO languages additionally bind a **receiver type** (`obj.method` → know what `obj` is). Each lang has its own receiver-type module (`<lang>/receiver_types.rs`) tracking local variable annotations and class-scope `this`/`self`. The receiver type is stored on the RawCall so downstream resolution can pick the correct overload when method names collide.
- Reflection / dynamic dispatch (`getattr(self, name)()`, JS dynamic `obj[k]()`, etc.) is **not** speculatively resolved; it lands as a `BlindSpot` record (per the project's "honest unknown beats fabricated edge" principle).
- Call edges (`RelType::Calls`) are the largest single edge type in the graph; the saturating-conversion helper `safe_row` in calls.rs guards against rows exceeding `u32::MAX` corrupting call-to-function attribution.

## 🏗️ Architecture

```
crates/
├── graph-nexus-core        # Zero-copy graph (rkyv), Incremental Caching, Graph Queries
├── graph-nexus-analyzer    # Tree-sitter parsers, BGE-M3 Embedder, HTTP Route Detector
└── graph-nexus-cli         # `gnx` binary, Tantivy BM25 Engine, Token-optimized Output
```

The analyzer streams parsed nodes through an MPSC channel into a single builder thread that assembles the graph, applies Route & Document extraction rules, and writes a zero-copy `.gitnexus-rs/graph.bin`. Read operations (like `context` and `query`) memory-map this file directly for zero-latency lookups.

## ⚙️ Tuning

| Env var | Default | Effect |
|---|---|---|
| `GNX_MAX_FILE_BYTES` | `16777216` (16 MiB) | Skip source files larger than this during ingest. Caps worst-case worker RAM at `num_threads × MAX`. Raise for legitimate generated/compiled-output indexing; lower on memory-constrained machines. |
| `GNX_EMBED_BATCH` | `32` | fastembed inference batch size. Lower to reduce peak resident during embedding (16 ≈ 200 MiB / 32 ≈ 300 MiB on BGE-M3 INT8). |
| `GNX_CSPROJ_MAX_DEPTH` | `4` | Directory recursion depth for `*.csproj` discovery. Raise for deeply-nested .NET monorepos. |
| `GNX_MODEL_CACHE` | `$HF_HUB_CACHE` ⤳ `$HF_HOME/hub` ⤳ `~/.cache/huggingface/hub` | Override the BGE-M3 model cache directory. |

## 📄 License

Licensed under [PolyForm Noncommercial 1.0.0](./LICENSE). Personal use, research,
hobby projects, and noncommercial organizations are explicitly permitted purposes.

**Commercial use is not granted by this license.** If you need commercial rights,
contact the upstream GitNexus author Abhigyan Patwari.

## 🙏 Acknowledgments

*   [GitNexus](https://github.com/abhigyanpatwari/GitNexus) by Abhigyan Patwari — original design, CLI surface, and conceptual model.
*   [tree-sitter](https://tree-sitter.github.io/) — robust incremental AST parsing.
*   [fastembed-rs](https://github.com/Anush008/fastembed-rs) — local ONNX inference engine.
*   [rkyv](https://rkyv.org/) — ultimate zero-copy deserialization.
*   [Tantivy](https://github.com/quickwit-oss/tantivy) — blazing fast Rust full-text search.
*   [BGE-M3 INT8](https://huggingface.co/MahradHosseini/bge-m3-onnx-int8) — High-quality community quantized multi-lingual model.
