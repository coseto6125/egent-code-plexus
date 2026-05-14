# Graph Nexus for LLM

A code intelligence graph built for **LLMs and AI code agents** — not for human IDE integration. Indexes any codebase across 14 languages in milliseconds, then answers structural questions like *who calls this*, *what's the blast radius if I change this function*, or *what's related to the auth flow*.

Built on top of [GitNexus](https://github.com/abhigyanpatwari/GitNexus) by [Abhigyan Patwari](https://github.com/abhigyanpatwari) — same core idea (a structural knowledge graph of a repo), rewritten in Rust for a different audience. Licensed under [PolyForm Noncommercial 1.0.0](./LICENSE).

> Required Notice: Copyright Abhigyan Patwari (https://github.com/abhigyanpatwari/GitNexus). Not affiliated with or endorsed by the upstream GitNexus project. Noncommercial use only. See [NOTICES.md](./NOTICES.md) for the full third-party attribution list.

## Why a Rust rewrite when upstream already exists?

GitNexus targets **human developers + IDE integration** — a long-running MCP server, rich Wiki rendering, and heuristic fallbacks (Jaccard similarity, etc.) that "guess" relationships when imports can't be statically resolved, so the graph stays coherent to a human reader.

graph-nexus targets **AI code agents** under a stricter contract:

- **Zero hallucination.** When the analyzer cannot resolve a binding, it records a `BlindSpot` and emits no edge. Agents prefer "I don't know" over a plausible-looking wrong answer — acting on a hallucinated dependency wastes more tokens than admitting uncertainty.
- **Stateless mmap, millisecond responses.** Each `gnx` invocation is a one-shot CLI: open `graph.bin` via `rkyv` zero-copy memory map, answer, exit. No background daemon, no warmup. An agent firing 30 queries in a single task pays only OS file-cache cost from the second query onward.
- **Token-economic output.** Output formats (`etoon`, `cypher`, compact JSON) are tuned for the LLM context window, not for human eyes. No tree-rendered UI, no syntax highlighting — only the minimum graph projection the agent needs.

Where GitNexus is a heavyweight knowledge-graph platform for developers, graph-nexus is a **surgical instrument for AI agents** — a millisecond-latency static-analysis retrieval engine.

Under the hood: zero-copy on-disk storage (rkyv + mmap), hybrid search (BM25 via Tantivy + BGE-M3 dense vectors), framework-aware route extraction. The CLI is `gnx`.

[繁體中文說明 (Traditional Chinese)](./README_zh-TW.md)

## 🚀 Key Features

*   **Blazing Fast & Zero-Copy**: Uses Tree-sitter + Rayon multi-threading for parsing, and `rkyv` for zero-copy memory-mapped `graph.bin` retrieval. It processes large repos in milliseconds.
*   **14 Languages Supported**: C, C#, C++, Dart, Go, Java, JavaScript, Kotlin, PHP, Python, Ruby, Rust, Swift, TypeScript.
*   **LLM-Native Output**: Emits extreme token-efficient formats ([TOON](https://crates.io/crates/etoon)) and concise string summaries. No hallucination-inducing formatting.
*   **Hybrid Search Engine**:
    *   **Semantic Search**: Uses **BGE-M3 INT8 Quantized Model** via `fastembed-rs` (`--embeddings`). Cross-lingual concept matching (e.g., search "Session Management" in Chinese, find English functions) with AVX2 CPU acceleration and massive memory reduction.
    *   **Lexical Search**: Uses **Tantivy (BM25)** for zero-latency, full-text tokenized keyword matching.
*   **Incremental Caching**: Only re-computes ASTs and Embeddings for modified files (SHA-256 Content Hash). Graph rebuilds drop from ~50s (Cold Start) to **< 0.25s**!
*   **Zero-Maintenance Route Extraction**: Purely based on RFC 7231 HTTP constants. Extracts API routes from both Declarative (e.g., `@Get`) and Imperative (e.g., `app.get()`) definitions across all languages.
*   **RAG Document Indexing**: Securely isolates `.md` (Markdown) and `.yaml` (GitHub Actions) files into parallel structures, parsing sections natively for LLM documentation retrieval without polluting the code execution graph.

## 📦 Installation

```bash
cargo install --git https://github.com/coseto6125/graph-nexus --bin gnx
```

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

## 🏗️ Architecture

```
crates/
├── graph-nexus-core        # Zero-copy graph (rkyv), Incremental Caching, Graph Queries
├── graph-nexus-analyzer    # Tree-sitter parsers, BGE-M3 Embedder, HTTP Route Detector
└── graph-nexus-cli         # `gnx` binary, Tantivy BM25 Engine, Token-optimized Output
```

The analyzer streams parsed nodes through an MPSC channel into a single builder thread that assembles the graph, applies Route & Document extraction rules, and writes a zero-copy `.gitnexus-rs/graph.bin`. Read operations (like `context` and `query`) memory-map this file directly for zero-latency lookups.

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
