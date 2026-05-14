# gnx-rs

> **非官方的 [GitNexus](https://github.com/abhigyanpatwari/GitNexus) Rust 重製版**  
>
> **專為 AI Agent 打造的圖譜化程式碼智慧引擎。** 將您的程式庫索引為知識圖譜，並透過 CLI 極速檢索。
>
> 原作：[Abhigyan Patwari](https://github.com/abhigyanpatwari)，基於
> [PolyForm Noncommercial 1.0.0](./LICENSE) 授權。
>
> 必備聲明: Copyright Abhigyan Patwari (https://github.com/abhigyanpatwari/GitNexus)
>
> 本專案與上游 GitNexus 無關聯亦未獲其背書。僅限非商業用途。

[English README](./README.md)

---

## 操作體驗進化：`gnx-rs` 與上游原版的比較

`gnx-rs` 繼承了 GitNexus 卓越的概念模型，但在底層執行架構上進行了徹底的顛覆。我們拔除了背景 Daemon，轉向基於 Rust 的零拷貝記憶體映射 (mmap) 架構。這不僅為了追求極致效能，更為了解決開發者與 LLM Agent (如 Claude, Cursor) 在日常工作流中所遭遇的痛點。

當您敲下 `gnx` 指令時，您將感受到以下巨大差異：

| 實際操作場景 (Workflow) | 原版 GitNexus (Node.js) | gnx-rs (Rust 版) |
| :--- | :--- | :--- |
| **啟動門檻 (Startup)** | 需要先啟動並維護背景 Daemon 伺服器 | **零阻力**。純 CLI 無狀態工具，隨用隨棄，不佔系統資源 |
| **圖譜更新 (`analyze`)** | 每次變更皆需全盤重建程式碼樹，耗時長 | **SHA-256 增量更新**。修改單一檔案只需 `< 0.25秒` 即可刷新圖譜 |
| **日常檢索 (`query`)** | 需要手動指定 `--mode` 切換語意或關鍵字 | **無縫混合 (RRF)**。一鍵並發雙引擎，自動將語意與 BM25 結果融合去重 |
| **上下文純粹度 (Context)** | 程式碼結果經常混雜無關的 Markdown 文件 | **RAG 文件隔離**。將程式碼與文檔清楚劃分為雙區塊，徹底消除 LLM 幻覺 |
| **變更偵測 (`review`)** | 依賴 Git 行號平移，易產生「沒改卻被標記」的誤判 | **純 AST 符號比對**。基於圖譜身分進行 Set Diff，100% 精準找出被改動的函數 |
| **微服務盤點 (`route-map`)**| 仰賴開發者寫死特定框架的特徵規則 | **通用 HTTP 推導**。基於 RFC 7231 常數，一鍵透視所有未知框架的 API |
| **LLM 耗用 Token 數** | 輸出大量冗長的 JSON 與不必要的括號結構 | **斷崖式下降 80%**。專為 LLM 視窗打造的單行 [TOON](https://crates.io/crates/etoon) 格式摘要 |

## 🚀 快速上手

```bash
# 從 GitHub 安裝
cargo install --git https://github.com/coseto6125/gnx-rs --bin gnx

# 1. 為當前專案建立程式碼圖譜 (極速，低於 1 秒)
gnx analyze --repo .

# 2. 建立附帶 BGE-M3 向量的圖譜 (初次執行會下載 ~540MB 的 INT8 模型)
gnx analyze --repo . --embeddings
```

## 支援的 14 種語言
C, C#, C++, Dart, Go, Java, JavaScript, Kotlin, PHP, Python, Ruby, Rust, Swift, TypeScript.

## 🏗️ 系統架構亮點

```
crates/
├── gnx-core        # 零拷貝圖譜定義 (rkyv)、增量快取演算法、圖譜檢索 helper
├── gnx-analyzer    # Tree-sitter 解析器、BGE-M3 向量生成、HTTP 路由偵測器
└── gnx-cli         # `gnx` 命令列、Tantivy BM25 全文引擎、Token 最佳化輸出
```

解析器 (Analyzer) 透過 MPSC 通道將 AST 節點傳遞給單一的 Builder 執行緒。Builder 負責組裝圖譜、推導 API 路由與文件分類，最後將其序列化為零拷貝的 `.gitnexus-rs/graph.bin`。讀取端（如 `context` 或 `query`）透過 mmap 直接映射硬碟檔案，達成零延遲查詢。

## 📄 授權條款

基於 [PolyForm Noncommercial 1.0.0](./LICENSE) 授權。明確允許個人使用、學術研究、業餘專案與非營利組織。

**本授權不允許商業使用。** 如需商業授權，請聯繫上游 GitNexus 原作者 Abhigyan Patwari。

## 🙏 致謝名單

*   [GitNexus](https://github.com/abhigyanpatwari/GitNexus) by Abhigyan Patwari — 原始設計與概念模型。
*   [tree-sitter](https://tree-sitter.github.io/) — 強健的增量 AST 解析。
*   [fastembed-rs](https://github.com/Anush008/fastembed-rs) — 本地 ONNX 向量推論引擎。
*   [rkyv](https://rkyv.org/) — 終極的零拷貝序列化套件。
*   [Tantivy](https://github.com/quickwit-oss/tantivy) — 極速 Rust 全文檢索引擎。