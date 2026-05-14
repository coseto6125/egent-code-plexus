# Graph Nexus for LLM

給 **LLM 與 AI 程式碼代理（AI code agents）** 用的代碼智能圖譜 — **不是給人類 IDE 整合用的**。十幾種語言、毫秒級建圖，然後可以問它「誰呼叫了這個」、「我改這個函式的爆炸半徑多大」、「跟 auth flow 相關的有哪些」這類結構性問題。

致敬 [GitNexus](https://github.com/abhigyanpatwari/GitNexus)（原作：[Abhigyan Patwari](https://github.com/abhigyanpatwari)）— 同樣的核心想法（repo 的結構化知識圖譜），用 Rust 重寫成面向**另一群受眾**的版本。基於 [PolyForm Noncommercial 1.0.0](./LICENSE) 授權。

> 必備聲明: Copyright Abhigyan Patwari (https://github.com/abhigyanpatwari/GitNexus)。本專案與上游 GitNexus 無關聯亦未獲其背書。僅限非商業用途。完整第三方授權清單見 [NOTICES.md](./NOTICES.md)。

## 跟上游 GitNexus 的差別

> **不是 drop-in 替代品。** 上游是範圍更大的 Node/TypeScript agent platform（MCP server、resources、hooks、generated skills）；graph-nexus 是無狀態 Rust CLI，專為 shell-mediated LLM 調用優化 — 不同 scope、不同 trade-off。

| 維度 | GitNexus | graph-nexus | 為什麼對 LLM agent 更合適 |
|---|---|---|---|
| **受眾** | 人類開發者 + IDE 整合 | AI 程式碼代理 | 優化目標決定下面每一行 |
| **運行模式** | 長駐 MCP server | One-shot CLI、rkyv mmap zero-copy | 每次查詢亞秒級；agent 一個任務內可發 30+ 查詢、無 warm-up 成本 |
| **import 解析不出來時** | 用啟發式（Jaccard 等）「猜」邊界讓圖連貫 | 記 `BlindSpot`、**不發邊** — 絕不憑空捏造 | Agent 不會誤信幻覺依賴；老實的「我不知道」比自信的錯答更省 token |
| **輸出格式** | Wiki / UI 豐富渲染 | `etoon` / `cypher` / compact JSON | 沒有 UI 樣板吃 context window，token 全花在圖本身 |
| **支援語言數** | 14 (TypeScript, JavaScript, Python, Java, Kotlin, C#, Go, Rust, PHP, Ruby, Swift, C, C++, Dart) | 31 — 上面 14 種 + Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig | Mixed-stack repo（DevOps config / Web3 合約 / infra-as-code）不再是盲區 |

> 語言深度有差。graph-nexus 在 31 種語言做結構層級（function / class / method / imports）解析，但**還沒**追上 GitNexus 在每種語言提供的完整 9 維度覆蓋（Named Bindings、Heritage、Constructor Inference、Config 等）。31 是廣度，不是 parity。

### 工具與整合對照

| LLM 面向 | 原版 GitNexus (`._source_code`) | Graph Nexus Rust (`gnx`) |
| :--- | :--- | :--- |
| **Agent 整合** | MCP server、resources、prompts、setup、hooks、generated skills | 無狀態 CLI，可透過 shell/tool wrapper 調用。**目前沒有內建 MCP server。** |
| **核心查詢工具** | `query`, `context`, `impact`, `detect_changes`, `rename`, `cypher`, group tools | `query`, `context`, `impact`, `detect-changes`, `route-map`, `cypher`, `summarize`, `rename` |
| **Context 輸出** | 完整的 MCP responses 與 repo skills | 精簡 `toon`/JSON/text，適合 shell-mediated LLM 調用 |
| **搜尋** | 文件化的 BM25 + semantic + RRF 混合 | 有 embedding 走混合；沒有就 fallback Tantivy BM25 |
| **Runtime / 儲存** | Node.js + LadybugDB | Rust + mmap `rkyv` graph 檔 |
| **最適合場景** | 有強 MCP/editor 整合的 agent runtime | 想要小執行檔、少零件的 local LLM harness / scripts |

底層細節：rkyv + mmap 的 zero-copy 硬碟儲存、Tantivy BM25 + BGE-M3 dense vector 混合檢索、框架路由自動抽取。CLI 命令是 `gnx`。

[English README](./README.md)

## 🚀 核心亮點

*   **極速與零拷貝 (Zero-Copy)**：冷啟動索引 `.sample_repo` — **22,772 檔、25 種偵測到的語言，僅 4.9 秒**（Java 3535、PHP 2907、TypeScript 1704、C# 945、Rust 870、C 801、Markdown 783、Dart 616、Bash 487、C++ 476、JavaScript 466、Solidity 403、Move 367、YAML 343、Ruby 156、Python 134、Swift 105、Go 99、Crystal 72、Kotlin 49、Lua 32、Zig 31、Dockerfile 20、Docker Compose 8、SQL 4）。同一張 graph 的查詢延遲：cypher 9 ms · context 9 ms · impact 5–6 ms · route-map 13 ms · BM25 query 24 ms · summarize 38 ms · detect-changes 230 ms。硬體：**AMD Ryzen 9 9950X（WSL2 內 8 顆邏輯 CPU、11.7 GiB RAM）**、Linux 6.6.87。Tree-sitter + Rayon 平行解析、`rkyv` mmap 零拷貝 `graph.bin`。重現：`python scripts/benchmark_gnx.py`。
*   **LLM 原生輸出**：產出極度節省 Token 的格式（[TOON](https://crates.io/crates/etoon)）與簡潔的字串摘要，杜絕複雜 JSON 括號引發的 LLM 幻覺。
*   **混合檢索引擎 (Hybrid Search)**：
    *   **語意搜尋 (Semantic)**：透過 `fastembed-rs` (`--embeddings`) 載入 **BGE-M3 INT8 量化模型**。支援精準的跨語言概念對齊（例如：搜中文「會話管理」，精準命中英文的 `SessionInterface`），並利用 AVX2 指令集大幅降低 CPU 負載與記憶體。
    *   **全文關鍵字 (Lexical)**：內建 **Tantivy (BM25)** 搜尋引擎，提供零延遲的精確關鍵字分詞比對。
*   **增量快取 (Incremental Caching)**：透過 SHA-256 檔案雜湊比對，只有被修改的檔案才會重新執行 AST 與神經網路運算。這讓圖譜重構時間從 50 秒（冷啟動）瞬間暴跌至 **小於 0.25 秒**！
*   **零維護的路由萃取 (Route Extraction)**：拋棄寫死框架名稱的過度設計，純粹依賴 RFC 7231 HTTP 標準協定常數。完美兼容聲明式（如 `@Get`）與指令式（如 `app.get()`）寫法，一鍵透視微服務全域 API。
*   **RAG 文件獨立索引**：安全地將 Markdown (`.md`) 與 GitHub Actions (`.yaml`) 隔離至專屬的文件陣列，並原生解析標題段落 (`Section`)。這讓 LLM 能夠精準查閱架構文件，又不會污染程式碼的執行流。

## 📦 安裝

| 平台 / 對象 | 指令 | 備註 |
| :--- | :--- | :--- |
| macOS Homebrew | `brew tap coseto6125/tap && brew install graph-nexus` | tap formula 公開後可用。Package：`graph-nexus`；binary：`gnx` |
| Linux / macOS | `curl -sSfL https://github.com/coseto6125/graph-nexus/releases/latest/download/install.sh \| sh` | 安裝預編 GitHub Release 二進位 |
| Windows PowerShell | `irm https://github.com/coseto6125/graph-nexus/releases/latest/download/install.ps1 \| iex` | 安裝預編 GitHub Release 二進位 |
| Rust 原始碼 build | `cargo install --git https://github.com/coseto6125/graph-nexus --bin gnx` | crates.io 發布前的安裝方式 |
| 手動下載 | [GitHub Releases](https://github.com/coseto6125/graph-nexus/releases) | 挑選對應 target 的 archive 並驗證 `.sha256` |

> `cargo install graph-nexus` 故意不列：crates.io publish 仍被卡，要等所有 analyzer grammar 依賴都能在 crates.io 上發布。

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

## 語言矩陣

graph-nexus 與上游共有的 14 種語言，每個 cell 直接對照上游宣稱的支援度 vs 我們實際 audit 結果（`crates/graph-nexus-analyzer/src/<lang>/`）。

**圖例**：
- ✓ &nbsp;上游有、我們也有
- ✅ &nbsp;**上游沒宣稱，我們有**（我們贏的地方）
- ⚠️ &nbsp;**上游有，我們缺或部分**（我們落後的地方）
- — &nbsp;雙方都沒有

| 語言 | Imports | Named | Exports | Heritage | Types | Ctor | Config | Frameworks | Entry |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| TypeScript | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| JavaScript | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | ⚠️ | ✓ |
| Python | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Java | ✓ | ⚠️ | ✓ | ✓ | ✓ | ⚠️ | ✅ | ✓ | ⚠️ |
| Kotlin | ✓ | ✓ | ✓ | ✓ | ✓ | ⚠️ | ✅ | ⚠️ | ⚠️ |
| C# | ✓ | ✓ | ✓ | ✓ | ✓ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| Go | ✓ | ✅ | ✓ | ✓ | ⚠️ | ⚠️ | ✓ | ⚠️ | ⚠️ |
| Rust | ✓ | ✓ | ✓ | ✓ | ✓ | ⚠️ | ✅ | ✓ | ⚠️ |
| PHP | ✓ | ✓ | ✓ | ✅ | ✓ | ⚠️ | ⚠️ | ⚠️ | ✓ |
| Ruby | ✓ | — | ✓ | ✓ | — | ⚠️ | ✅ | ⚠️ | ✓ |
| Swift | ✅ | — | ✓ | ✓ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| C | ✅ | — | ✓ | ✅ | ⚠️ | ⚠️ | ✅ | ⚠️ | ⚠️ |
| C++ | ✅ | ✅ | ✓ | ✓ | ⚠️ | ⚠️ | ✅ | ⚠️ | ⚠️ |
| Dart | ✓ | ✅ | ✓ | ✓ | ⚠️ | ⚠️ | ✅ | ⚠️ | ⚠️ |

**我們超越上游的地方**（15 個 ✅）：C / C++ 拿到上游沒宣稱的 Imports & Heritage；Java/Kotlin/Rust/Ruby/Dart 拿到上游沒做的 toolchain Config 解析；PHP 拿到 Heritage；Go/C++/Dart 拿到 Named Bindings；Swift/C/C++ 拿到基本 Imports。

**我們落後上游的地方**（多數 ⚠️）：**Constructor Inference** 是最大缺口 — Python、PHP、Ruby 有完整的 receiver-type binding，其他 11 種都還是 partial。**Frameworks & Entry Points** 在 Kotlin / C# / Swift / C / C++ / Dart 沒接（上游全部有，我們 parser 在但沒接 framework helper）。

除了這 14 種以外，Rust 端還有 **17 個 provider**（Bash、Crystal、Cairo、Dockerfile、Docker Compose、GitHub Actions、HCL、Lua、Markdown、Move、Nim、Solidity、SQL、Verilog、Vyper、YAML、Zig）停留在結構層級 — 上游沒對應基準可比。

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
