# 市面盤點：Rust + tree-sitter + MCP + graph-for-LLM

[English](../competitive-landscape.md)

最後盤點：2026-05-22。

**這個空間在 2026 年異常擁擠**。光是過去 3 個月新出的 Rust 同類專案就 ≥15 個。沒人做我們做的全部，但每個維度都有人做。

## 最像 ecp 的 Top 5

| 專案 | Stars | License | 像在哪裡 | 跟我們差在哪裡 |
|---|---|---|---|---|
| **[codescope](https://github.com/onur-gokyildiz-bhi/codescope)** (onur-gokyildiz-bhi) | 21 | MIT | **最像**：Rust + MCP + 「graph-first not embeddings-first」+ ms 級 traversal + 用 rkyv（Cargo.lock 看到）+ 57 langs + 9 agent 整合 | SurrealDB backend、有 LSP mode + Web UI + daemon、沒 Process 抽象、沒 Leiden |
| **[codesight-mcp](https://github.com/cmillstead/codesight-mcp)** | — | — | 66 langs tree-sitter、34 MCP tools、impact analysis | 無 community detection、focus 在 retrieval 不在 graph algo |
| **[narsil-mcp](https://github.com/postrv/narsil-mcp)** (postrv) | — | — | 32 langs、90 MCP tools、call graph、security scanning | 沒 community detection |
| **[rhizome](https://github.com/basidiocarp/rhizome)** (basidiocarp) | — | — | tree-sitter + LSP 雙 backend、sub-ms parse | 不做 graph storage 層，更像 LSP wrapper |
| **[qartez-mcp](https://github.com/kuberstar/qartez-mcp)** | — | — | 37 langs（tree-sitter + regex fallback）、project map、symbol search、impact analysis | 不做 community |
| **[coraline](https://github.com/greysquirr3l/coraline)** | 10 | Apache-2.0 | 28 langs、MCP、sub-second indexing | SQLite backend，沒 Leiden |
| **[shaharia-lab/code-navigator](https://github.com/shaharia-lab/code-navigator)** | 5 | MIT | 「compressed graph」for AI agents、impact analysis | 還在早期 |
| **[Jakedismo/codegraph-rust](https://github.com/Jakedismo/codegraph-rust)** | 754 | unclear | 大 star、14 crates | 5 個月沒 push、SurrealDB、不做 community、走 agent framework 路線 |

## Adjacent（部分重疊但定位不同）

| 專案 | 重點 |
|---|---|
| **[github/stack-graphs](https://github.com/github/stack-graphs)** | 877 stars，GitHub 官方 tree-sitter 跨檔符號解析，只解 cross-ref 不做 community / Process |
| **[probe](https://github.com/probelabs/probe)** | ripgrep 速度 + tree-sitter AST，semantic code search，沒 graph storage |
| **[code-sage](https://github.com/faxioman/code-sage)** | BM25 + 向量 + tree-sitter chunking，semantic search 不是 graph |
| **[codesearch](https://github.com/flupkede/codesearch)** | hybrid 向量 + BM25 + tree-sitter chunking |
| **[semtree](https://github.com/rustkit-ai/semtree)** | tree-sitter + embeddings + RAG multi-backend |
| **[nusy-codegraph](https://github.com/hankh95/nusy-codegraph)** | Arrow-native code object storage（有趣的 storage 角度） |

## 結論

**完全和我們一樣的：沒有**。

最接近 ecp 設計取向的是 **codescope**：同樣 Rust-native、graph-first、ms 級 query、用 rkyv、本地不依賴雲。但他們：
- backend 是 SurrealDB（我們是純 rkyv mmap 檔，更輕）
- 沒做 community detection / Process 抽象（**這是 ecp 真正的差異化**）
- 多了 LSP server + Web UI + daemon mode（功能比我們廣，但偏「平台」不偏「演算法」）

ecp 在這個擁擠的賽道**唯一的真差異化**：

| 別人做的（同質化） | ecp 獨有 |
|---|---|
| tree-sitter parse（30-66 langs） | **Leiden community detection → Process 節點抽象**（LLM 直接拿到「execution flow」級語義，不只 callee/caller） |
| impact analysis（callers/callees） | **deterministic seeded 輸出**（同 corpus 同 seed bit-identical） |
| MCP tool wrap | **零拷貝 rkyv mmap**（codescope 也用 rkyv，但是 transitive；他們主存儲是 SurrealDB） |
| BM25/vector hybrid | **Cypher query 語法**（少數做的人） |

## 借鑒清單（小但具體）

| 從誰借 | 借什麼 | 投入成本 |
|---|---|---|
| **codescope** | `codescope insight`「per-repo + hourly activity」概念 — 給 user 看哪些 MCP tool 被叫多少次（observability） | 低，純 telemetry |
| **codescope / Jakedismo** | LSP bridge **作為 opt-in feature**（不是必要） — 解決 C++ template / Java generic 等 tree-sitter 解不開的 cross-ref | 中，會引入 LSP cold start 成本，需 feature-gated |
| **nusy-codegraph** | Arrow-native storage 思路 — 跟 rkyv 都是零拷貝，但 Arrow 跨語言生態大（Python pandas 直讀）；若要做 Python wheel binding 用得到 | 高，等有 user demand 再談 |
| **codescope / coraline** | "sub-second indexing" demo benchmark **作為標準對照** — 跟他們同 corpus 跑數據，公開比 | 低，但要做 marketing |

## 不該借的

- ❌ **SurrealDB backend**（codescope、Jakedismo） — query 走 DB 引擎跟我們 <30ms 目標衝突
- ❌ **AI / RAG / embedding pipeline 整合進 core**（Jakedismo、semtree） — 我們的核心競爭力是 deterministic 不是 fuzzy
- ❌ **LSP 為 default**（rhizome） — LSP cold start 殺 <5s 目標
- ❌ **massive MCP tool 數量**（narsil 90、codesight 34、codescope 32 tools） — tool 多 ≠ tool 好，每個 tool 都要維護一份文檔給 LLM 讀

## 真正應該繼續做的

我們的差異化在 **「圖演算法層做出語義 abstraction」**（Leiden → Process），不在工具量、語言量、agent 整合廣度。**這個方向繼續深挖比鋪 LSP/embedding/agent 整合 ROI 高**。

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
