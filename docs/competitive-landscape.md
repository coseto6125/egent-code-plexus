# Field survey: Rust + tree-sitter + MCP + graph-for-LLM

[繁體中文 (Traditional Chinese)](./readme_i18n/competitive-landscape_zh-TW.md)

Last surveyed: 2026-05-22.

**This space is unusually crowded in 2026.** ≥15 similar Rust projects pushed to GitHub in the last 3 months alone. Nobody does the whole stack we do, but every individual axis has someone working on it.

## Top 5 closest to ecp

| Project | Stars | License | Where it overlaps | Where it diverges |
|---|---|---|---|---|
| **[codescope](https://github.com/onur-gokyildiz-bhi/codescope)** (onur-gokyildiz-bhi) | 21 | MIT | **Closest match**: Rust + MCP + "graph-first not embeddings-first" + ms-level traversal + rkyv (in Cargo.lock) + 57 languages + 9 agent integrations | SurrealDB backend, LSP mode + Web UI + daemon, no Process abstraction, no Leiden |
| **[codesight-mcp](https://github.com/cmillstead/codesight-mcp)** | — | — | 66 langs via tree-sitter, 34 MCP tools, impact analysis | No community detection, focus on retrieval not graph algorithms |
| **[narsil-mcp](https://github.com/postrv/narsil-mcp)** (postrv) | — | — | 32 langs, 90 MCP tools, call graph, security scanning | No community detection |
| **[rhizome](https://github.com/basidiocarp/rhizome)** (basidiocarp) | — | — | tree-sitter + LSP dual backend, sub-ms parse | No graph storage layer — closer to an LSP wrapper |
| **[qartez-mcp](https://github.com/kuberstar/qartez-mcp)** | — | — | 37 langs (tree-sitter + regex fallback), project map, symbol search, impact analysis | No community detection |
| **[coraline](https://github.com/greysquirr3l/coraline)** | 10 | Apache-2.0 | 28 langs, MCP, sub-second indexing | SQLite backend, no Leiden |
| **[shaharia-lab/code-navigator](https://github.com/shaharia-lab/code-navigator)** | 5 | MIT | "Compressed graph" for AI agents, impact analysis | Still early-stage |
| **[Jakedismo/codegraph-rust](https://github.com/Jakedismo/codegraph-rust)** | 754 | unclear | High star count, 14 crates | 5 months stale, SurrealDB, no community detection, focused on agent framework |

## Adjacent (overlapping but differently positioned)

| Project | Focus |
|---|---|
| **[github/stack-graphs](https://github.com/github/stack-graphs)** | 877 stars. GitHub's official Rust tree-sitter cross-file symbol resolver. Cross-references only — no community / Process |
| **[probe](https://github.com/probelabs/probe)** | ripgrep speed + tree-sitter AST, semantic code search, no graph storage |
| **[code-sage](https://github.com/faxioman/code-sage)** | BM25 + vector + tree-sitter chunking, semantic search not a graph |
| **[codesearch](https://github.com/flupkede/codesearch)** | hybrid vector + BM25 + tree-sitter chunking |
| **[semtree](https://github.com/rustkit-ai/semtree)** | tree-sitter + embeddings + RAG multi-backend |
| **[nusy-codegraph](https://github.com/hankh95/nusy-codegraph)** | Arrow-native code object storage (interesting storage angle) |

## Conclusion

**Nothing does exactly what we do.**

The design closest to ecp is **codescope**: Rust-native, graph-first, ms-level queries, uses rkyv, fully local. But they:
- Use SurrealDB as backend (we're pure rkyv mmap file — lighter)
- Don't do community detection / Process abstraction (**this is ecp's real differentiator**)
- Ship LSP server + Web UI + daemon mode (broader product, but "platform" oriented rather than "algorithm" oriented)

ecp's **only true differentiator** in this crowded space:

| The commodity layer (what everyone does) | ecp's distinct bet |
|---|---|
| tree-sitter parse (30-66 langs) | **Leiden community detection → Process node abstraction** (LLMs get "execution flow" level semantics, not just callee/caller) |
| impact analysis (callers/callees) | **Deterministic seeded output** (same corpus + same seed → bit-identical) |
| MCP tool wrap | **Zero-copy rkyv mmap** (codescope also uses rkyv, but transitively; their primary store is SurrealDB) |
| BM25/vector hybrid | **Cypher query language** (few do this) |

## Borrow list (small, specific)

| From | What | Cost |
|---|---|---|
| **codescope** | `codescope insight` "per-repo + hourly activity" pattern — gives users visibility into which MCP tools agents actually invoke (observability) | Low, pure telemetry |
| **codescope / Jakedismo** | LSP bridge **as an opt-in feature** (not required) — solves tree-sitter blind spots (C++ templates, Java generics, etc.) | Medium — introduces LSP cold-start cost, must be feature-gated |
| **nusy-codegraph** | Arrow-native storage angle — same zero-copy as rkyv but with broader cross-language ecosystem (Python pandas can mmap-read directly); relevant only if a Python wheel binding becomes a need | High, defer until user demand |
| **codescope / coraline** | "sub-second indexing" demo benchmark **as a standard comparison** — same corpus, side-by-side numbers, publish | Low engineering effort, but marketing follow-through needed |

## Do not borrow

- ❌ **SurrealDB backend** (codescope, Jakedismo) — query path through a DB engine conflicts with our <30ms target
- ❌ **AI / RAG / embedding pipeline integrated into core** (Jakedismo, semtree) — our core differentiator is deterministic, not fuzzy
- ❌ **LSP as default** (rhizome) — LSP cold start kills the <5s cold-ingest target
- ❌ **Massive MCP tool counts** (narsil 90, codesight 34, codescope 32 tools) — more tools ≠ better tools; every tool is a documented contract the consuming LLM has to read

## What we should actually keep doing

Our differentiation lives in **"making semantic abstractions at the graph algorithm layer"** (Leiden → Process) — not in tool count, language count, or breadth of agent integration. **Continuing to deepen this axis has higher ROI than chasing LSP / embedding / agent-framework integration parity.**

## Sources

- [onur-gokyildiz-bhi/codescope](https://github.com/onur-gokyildiz-bhi/codescope)
- [postrv/narsil-mcp](https://github.com/postrv/narsil-mcp)
- [flupkede/codesearch](https://github.com/flupkede/codesearch)
- [kuberstar/qartez-mcp](https://github.com/kuberstar/qartez-mcp)
- [basidiocarp/rhizome](https://github.com/basidiocarp/rhizome)
- [cmillstead/codesight-mcp](https://github.com/cmillstead/codesight-mcp)
- [faxioman/code-sage](https://github.com/faxioman/code-sage)
- [probelabs/probe](https://github.com/probelabs/probe)
- [github/stack-graphs](https://github.com/github/stack-graphs)
- [Jakedismo/codegraph-rust](https://github.com/Jakedismo/codegraph-rust)
- [greysquirr3l/coraline](https://github.com/greysquirr3l/coraline)
- [shaharia-lab/code-navigator](https://github.com/shaharia-lab/code-navigator)
- [hankh95/nusy-codegraph](https://github.com/hankh95/nusy-codegraph)
