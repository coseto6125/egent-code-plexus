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

All commands accept `--format text|json|toon`. The default for query is a highly token-optimized text format.

## Language Matrix

For the 14 languages graph-nexus shares with upstream, here's the per-cell delta from an evidence-based audit. Each cell compares upstream GitNexus's claimed support against our actual implementation in `crates/graph-nexus-analyzer/src/<lang>/`.

**Legend**:
- ✓ &nbsp;both upstream and graph-nexus support this
- ✅ &nbsp;**upstream doesn't claim it, graph-nexus does** (where we go beyond upstream)
- ⚠️ &nbsp;**upstream claims it, graph-nexus is missing or partial** (where we lag)
- — &nbsp;neither claims it

| Language | Imports | Named | Exports | Heritage | Types | Ctor | Config | Frameworks | Entry |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| TypeScript | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| JavaScript | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | ⚠️ | ✓ |
| Python | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Java | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✅ | ✓ | ✓ |
| Kotlin | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✅ | ⚠️ | ✓ |
| C# | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ⚠️ | ✓ |
| Go | ✓ | ✅ | ✓ | ✓ | ⚠️ | ✓ | ✓ | ⚠️ | ✓ |
| Rust | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✅ | ✓ | ✓ |
| PHP | ✓ | ✓ | ✓ | ✅ | ✓ | ✓ | ⚠️ | ⚠️ | ✓ |
| Ruby | ✓ | — | ✓ | ✓ | — | ✓ | ✅ | ⚠️ | ✓ |
| Swift | ✅ | — | ✓ | ✓ | ⚠️ | ✓ | ⚠️ | ⚠️ | ✓ |
| C | ✅ | — | ✓ | ✅ | ⚠️ | ✓ | ✅ | ⚠️ | ✓ |
| C++ | ✅ | ✅ | ✓ | ✓ | ⚠️ | ✓ | ✅ | ⚠️ | ✓ |
| Dart | ✓ | ✅ | ✓ | ✓ | ⚠️ | ✓ | ✅ | ⚠️ | ✓ |

**Where graph-nexus goes beyond upstream** (15 ✅ cells): C/C++ get Imports & Heritage that upstream doesn't claim; Java/Kotlin/Rust/Ruby/Dart get Config parsing for toolchains upstream doesn't cover; PHP gets Heritage; Go/C++/Dart get Named Bindings; Swift/C/C++ get basic Imports.

**Wave 1 closed 28 cells** (Constructor Inference rolled out to all 14 languages mirroring Python's `4e4fb1b` receiver-type binding prototype; Java static-import named bindings; C# `csproj`/`global.json` config; Exports for Go/Ruby/C/Dart per language conventions; cross-language Entry Point scorer combining routes + `main()` + framework decorators). **Remaining ⚠️ (17 cells, Wave 2 targets)**: Frameworks across 10 langs (JS, Kotlin, C#, Go, PHP, Ruby, Swift, C, C++, Dart); Types for Go / Swift / Dart / C / C++; Config for PHP and Swift.

**Matrix-opt batch (HEAD `86e65a7`)** deepened existing ✓ cells: Go gained per-struct-field visibility, Dart per-symbol underscore convention, Ruby `attr_*` metaprogramming + `include`/`extend` mixin tracking, TS/JS re-export alias preservation. See `docs/specs/2026-05-15-matrix-optimization-opportunities.md`.

Beyond these 14, the Rust providers also cover **17 additional languages** (Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig) at the structural level. The matrix-opt batch added concrete dimensions to three of these: Bash now records `source`/`.` imports, Lua tracks `local M = require()` aliases + metatable inheritance + table-assigned methods, Solidity classifies state-variable visibility.

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
