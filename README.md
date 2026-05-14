# gnx-rs

> **Unofficial Rust reimplementation of [GitNexus](https://github.com/abhigyanpatwari/GitNexus)**
> 
> **Graph-powered code intelligence for AI agents.** Index any codebase into a knowledge graph, then query it via CLI.
>
> by [Abhigyan Patwari](https://github.com/abhigyanpatwari), licensed under
> [PolyForm Noncommercial 1.0.0](./LICENSE).
>
> Required Notice: Copyright Abhigyan Patwari (https://github.com/abhigyanpatwari/GitNexus)
>
> Not affiliated with or endorsed by the upstream GitNexus project. Noncommercial use only.

[繁體中文說明 (Traditional Chinese)](./README_zh-TW.md)

---

## The Workflow Upgrade: `gnx-rs` vs Upstream

`gnx-rs` takes the phenomenal conceptual model of GitNexus and completely reimagines the execution architecture. By stripping out the background daemon and shifting to a zero-copy memory-mapped structure in Rust, it delivers a drastically better day-to-day experience for both human developers and LLM Agents (Claude, Cursor, etc.).

Here is what changes when you type `gnx` instead of `gitnexus`:

| Workflow & Experience | Upstream GitNexus (Node.js) | gnx-rs (Rust) |
| :--- | :--- | :--- |
| **Startup Friction** | Requires starting and maintaining a background Daemon server before use. | **Zero Friction**. A stateless CLI. Runs instantly and terminates immediately. |
| **Graph Updates (`analyze`)** | Full codebase rebuild on every change, taking significant time. | **SHA-256 Incremental Updates**. Changing a single file takes `< 0.25s` to rebuild the graph. |
| **Search Engine (`query`)** | Manual toggle required (`--mode semantic` or `bm25`). | **Automatic Hybrid RRF**. Simultaneously runs Vector + BM25 search, automatically fusing results. |
| **Context Purity** | Often returns irrelevant Markdown files mixed with code symbols. | **RAG Isolation**. Code symbols and Markdown/YAML docs are returned in clean, separated blocks. |
| **Blast Radius (`review`)** | Diff line-number shifting can cause phantom false-positive changes. | **Set-Diff Symbol Matching**. 100% accurate change detection based on AST identity, immune to line shifts. |
| **API Route Maps** | Relies on hardcoded, framework-specific matchers. | **Universal HTTP Deduction**. Extracts routes based on RFC 7231 constants, working across unknown frameworks. |
| **LLM Token Economy** | Outputs verbose, nested JSON structures. | **80% Token Reduction**. Emits ultra-condensed [TOON](https://crates.io/crates/etoon) format designed specifically for LLM context windows. |

## Quick Start

```bash
cargo install --git https://github.com/coseto6125/gnx-rs --bin gnx

# 1. Build a code graph for the current repo (Extremely fast, < 1s)
gnx analyze --repo .

# 2. Build with BGE-M3 Semantic Embeddings (Downloads ~540MB INT8 model on first run)
gnx analyze --repo . --embeddings
```

## Supported Languages (14)
C, C#, C++, Dart, Go, Java, JavaScript, Kotlin, PHP, Python, Ruby, Rust, Swift, TypeScript.

## Architecture Highlights

```
crates/
├── gnx-core        # Zero-copy graph (rkyv), Incremental Caching, Graph Queries
├── gnx-analyzer    # Tree-sitter parsers, BGE-M3 Embedder, HTTP Route Detector
└── gnx-cli         # `gnx` binary, Tantivy BM25 Engine, Token-optimized Output
```

The analyzer streams parsed nodes through an MPSC channel into a single builder thread that assembles the graph, applies Route & Document extraction rules, and writes a zero-copy `.gitnexus-rs/graph.bin`. Read operations (like `context` and `query`) memory-map this file directly for zero-latency lookups.

## License

Licensed under [PolyForm Noncommercial 1.0.0](./LICENSE). Personal use, research,
hobby projects, and noncommercial organizations are explicitly permitted purposes.

**Commercial use is not granted by this license.** If you need commercial rights,
contact the upstream GitNexus author Abhigyan Patwari.

## Acknowledgments

- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) by Abhigyan Patwari — original design, CLI surface, and conceptual model
- [tree-sitter](https://tree-sitter.github.io/) — incremental parsing
- [fastembed-rs](https://github.com/Anush008/fastembed-rs) — local ONNX inference for BGE-M3
- [rkyv](https://rkyv.org/) — zero-copy deserialization
- [Tantivy](https://github.com/quickwit-oss/tantivy) — blazing fast Rust full-text search
