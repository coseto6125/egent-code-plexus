# Graph Nexus for LLM

給 **LLM 與 AI 程式碼代理（AI code agents）** 用的代碼智能圖譜 — **不是給人類 IDE 整合用的**。十幾種語言、毫秒級建圖，然後可以問它「誰呼叫了這個」、「我改這個函式的爆炸半徑多大」、「跟 auth flow 相關的有哪些」這類結構性問題。

致敬 [GitNexus](https://github.com/abhigyanpatwari/GitNexus)（原作：[Abhigyan Patwari](https://github.com/abhigyanpatwari)）— 同樣的核心想法（repo 的結構化知識圖譜），用 Rust 重寫成面向**另一群受眾**的版本。基於 [PolyForm Noncommercial 1.0.0](./LICENSE) 授權。

> 必備聲明: Copyright Abhigyan Patwari (https://github.com/abhigyanpatwari/GitNexus)。本專案與上游 GitNexus 無關聯亦未獲其背書。僅限非商業用途。完整第三方授權清單見 [NOTICES.md](./NOTICES.md)。

## 跟上游 GitNexus 的差別

| 維度 | GitNexus | graph-nexus | 為什麼對 LLM agent 更合適 |
|---|---|---|---|
| **受眾** | 人類開發者 + IDE 整合 | AI 程式碼代理 | 優化目標決定下面每一行 |
| **運行模式** | 長駐 MCP server | One-shot CLI、rkyv mmap zero-copy | 每次查詢亞秒級；agent 一個任務內可發 30+ 查詢、無 warm-up 成本 |
| **import 解析不出來時** | 用啟發式（Jaccard 等）「猜」邊界讓圖連貫 | 記 `BlindSpot`、**不發邊** — 絕不憑空捏造 | Agent 不會誤信幻覺依賴；老實的「我不知道」比自信的錯答更省 token |
| **輸出格式** | Wiki / UI 豐富渲染 | `etoon` / `cypher` / compact JSON | 沒有 UI 樣板吃 context window，token 全花在圖本身 |
| **支援語言數** | 14 (TypeScript, JavaScript, Python, Java, Kotlin, C#, Go, Rust, PHP, Ruby, Swift, C, C++, Dart) | 31 — 上面 14 種 + Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig | Mixed-stack repo（DevOps config / Web3 合約 / infra-as-code）不再是盲區 |

> 語言深度有差。graph-nexus 在 31 種語言做結構層級（function / class / method / imports）解析，但**還沒**追上 GitNexus 在每種語言提供的完整 9 維度覆蓋（Named Bindings、Heritage、Constructor Inference、Config 等）。31 是廣度，不是 parity。

底層細節：rkyv + mmap 的 zero-copy 硬碟儲存、Tantivy BM25 + BGE-M3 dense vector 混合檢索、框架路由自動抽取。CLI 命令是 `gnx`。

[English README](./README.md)

## 🚀 核心亮點

*   **極速與零拷貝 (Zero-Copy)**：結合 Tree-sitter 與 Rayon 多執行緒進行語法分析，並使用 `rkyv` 打造 Zero-copy 的記憶體映射 (mmap) `graph.bin`。解析超大型專案只需不到一秒鐘。
*   **支援 14 種語言**：C, C#, C++, Dart, Go, Java, JavaScript, Kotlin, PHP, Python, Ruby, Rust, Swift, TypeScript。
*   **LLM 原生輸出**：產出極度節省 Token 的格式（[TOON](https://crates.io/crates/etoon)）與簡潔的字串摘要，杜絕複雜 JSON 括號引發的 LLM 幻覺。
*   **混合檢索引擎 (Hybrid Search)**：
    *   **語意搜尋 (Semantic)**：透過 `fastembed-rs` (`--embeddings`) 載入 **BGE-M3 INT8 量化模型**。支援精準的跨語言概念對齊（例如：搜中文「會話管理」，精準命中英文的 `SessionInterface`），並利用 AVX2 指令集大幅降低 CPU 負載與記憶體。
    *   **全文關鍵字 (Lexical)**：內建 **Tantivy (BM25)** 搜尋引擎，提供零延遲的精確關鍵字分詞比對。
*   **增量快取 (Incremental Caching)**：透過 SHA-256 檔案雜湊比對，只有被修改的檔案才會重新執行 AST 與神經網路運算。這讓圖譜重構時間從 50 秒（冷啟動）瞬間暴跌至 **小於 0.25 秒**！
*   **零維護的路由萃取 (Route Extraction)**：拋棄寫死框架名稱的過度設計，純粹依賴 RFC 7231 HTTP 標準協定常數。完美兼容聲明式（如 `@Get`）與指令式（如 `app.get()`）寫法，一鍵透視微服務全域 API。
*   **RAG 文件獨立索引**：安全地將 Markdown (`.md`) 與 GitHub Actions (`.yaml`) 隔離至專屬的文件陣列，並原生解析標題段落 (`Section`)。這讓 LLM 能夠精準查閱架構文件，又不會污染程式碼的執行流。

## 📦 安裝

```bash
cargo install --git https://github.com/coseto6125/graph-nexus --bin gnx
```

安裝後，執行檔名稱為 `gnx`（在 crates.io 上的套件名為 `graph-nexus`）。

## ⚡ 使用方式

```bash
# 1. 為當前專案建立程式碼圖譜 (極速，低於 1 秒)
gnx analyze --repo .

# 2. 建立附帶 BGE-M3 向量的圖譜 (初次執行會下載 ~540MB 的 INT8 模型)
gnx analyze --repo . --embeddings

# 3. 混合檢索：語意與概念搜尋 (需要先執行 --embeddings)
gnx query --query "資料庫連線池設定"

# 4. 混合檢索：精確關鍵字 BM25 (使用 Tantivy)
gnx query --query "DatabaseConnection"

# 5. 一鍵萃取微服務中所有的 API 路由
gnx route-map --repo .

# 6. 尋找特定符號的爆炸半徑 / 上游呼叫鏈 (Refactor 前必備)
gnx impact --target validateUser --direction upstream

# 7. 探索上下文 (包含 Metadata、裝飾器、簽名)
gnx context --name validateUser
```

所有指令皆支援 `--format text|json|toon`。`query` 的預設輸出為極度優化的 `text` 格式。

## 🏗️ 系統架構

```
crates/
├── graph-nexus-core        # 零拷貝圖譜定義 (rkyv)、增量快取演算法、圖譜檢索 helper
├── graph-nexus-analyzer    # Tree-sitter 解析器、BGE-M3 向量生成、HTTP 路由偵測器
└── graph-nexus-cli         # `gnx` 命令列、Tantivy BM25 全文引擎、Token 最佳化輸出
```

解析器 (Analyzer) 透過 MPSC 通道將 AST 節點傳遞給單一的 Builder 執行緒。Builder 負責組裝圖譜、推導 API 路由與文件分類，最後將其序列化為零拷貝的 `.gitnexus-rs/graph.bin`。讀取端（如 `context` 或 `query`）透過 mmap 直接映射硬碟檔案，達成零延遲查詢。

## 📄 授權條款

基於 [PolyForm Noncommercial 1.0.0](./LICENSE) 授權。明確允許個人使用、學術研究、業餘專案與非營利組織。

**本授權不允許商業使用。** 如需商業授權，請聯繫上游 GitNexus 原作者 Abhigyan Patwari。
