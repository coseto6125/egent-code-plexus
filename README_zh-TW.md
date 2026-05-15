# Graph Nexus for LLM

給 **LLM 與 AI 程式碼代理（AI code agents）** 用的代碼智能圖譜 — **不是給人類 IDE 整合用的**。31 種語言、毫秒級建圖，然後可以問它「誰呼叫了這個」、「我改這個函式的爆炸半徑多大」、「跟 auth flow 相關的有哪些」這類結構性問題。

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

> **Pre-release 狀態**：首個 GitHub Release 發出前，預編安裝腳本會自動 fallback 到 `cargo install --git`。下面每個平台**今天**都至少有一條可用的 terminal 安裝路徑。

### 全平台通用（今天就能用，不需 Release）

```bash
cargo install --git https://github.com/coseto6125/graph-nexus --bin gnx --locked
```

需要 Rust toolchain（[rustup.rs](https://rustup.rs)）。原始碼編譯，首次數分鐘，之後 incremental 快。

### 各平台一鍵指令

| 平台 | 指令 | 備註 |
| :--- | :--- | :--- |
| **Linux / macOS** | `curl -sSfL https://raw.githubusercontent.com/coseto6125/graph-nexus/main/scripts/install.sh \| sh` | 先試預編 Release；沒 Release 時自動 fallback 到 `cargo install --git`。設 `GNX_FORCE_CARGO=1` 可跳過 Release 偵測。 |
| **Windows PowerShell** | `iwr https://raw.githubusercontent.com/coseto6125/graph-nexus/main/scripts/install.ps1 -UseBasicParsing \| iex` | 同樣 Release 優先 / cargo fallback。設 `$env:GNX_FORCE_CARGO='1'` 強制 cargo。 |
| **macOS Homebrew** | `brew tap coseto6125/tap && brew install graph-nexus` | 首個 Release 帶出 tap formula 後可用。 |
| **手動下載** | [GitHub Releases](https://github.com/coseto6125/graph-nexus/releases) | 挑選對應 target 的 archive 並驗證 `.sha256`。 |

> 安裝後 binary 是 `gnx`（crates.io 上的套件未來會是 `graph-nexus`）。`cargo install graph-nexus` 故意不列：crates.io publish 仍被卡，要等所有 analyzer grammar 依賴都能在 crates.io 上發布。

> 一旦有 tag 化 Release，安裝腳本同時也會從 `…/releases/latest/download/install.{sh,ps1}` 提供 — 兩種 URL 皆可用；`raw.githubusercontent.com` 形式只是在**首個 Release 之前**也能 work。

## ⚡ 使用方式

### 快速上手

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

每個讀取端命令都接受 `--format text|json|toon`。預設值為各命令最省 token 的表示：多數命令採 `toon`、`query` 採 `text`、`cypher`/`status`/`process` 採 `json`、`summarize`/`doctor` 採 `md`/`compact`。

### 任務 → 命令對照

| 目的 | 用什麼 |
|---|---|
| 為全新專案建立索引 | `gnx analyze --repo .`（或在專案內 `gnx analyze-here`） |
| 修改檔案後更新 | 同上 — `analyze` 走 SHA-256 內容雜湊增量 |
| 符號是否存在？在哪？ | `gnx query --query <name>` |
| 取得符號的 metadata、呼叫者、被呼叫者 | `gnx context --name <name>` |
| 編輯 X 會壞掉什麼？ | `gnx impact --target <name> --direction upstream` |
| X 依賴了什麼？ | `gnx impact --target <name> --direction downstream` |
| 圖譜任意 traversal / 取原始碼 body | `gnx cypher 'MATCH (m:Method) WHERE … RETURN m.content'` |
| 列出全部 HTTP route | `gnx route-map` |
| 誰呼叫 `POST /api/users`？ | `gnx api-impact --route /api/users --method POST` |
| 何處呼叫外部 HTTP / DB / Redis / queue？ | `gnx tool-map [--category http,db,redis,queue]` |
| 追蹤單一執行流（從起點到終點） | `gnx process --name <name>` |
| 架構摘要 / 熱門檔案 / 重要符號 | `gnx summarize` |
| 框架覆蓋率與盲區報表 | `gnx doctor` |
| 這個 commit 改了什麼 + 影響範圍 | `gnx detect-changes --scope compare --base-ref HEAD~1` |
| 跨檔安全 rename 一個符號（14 語言 — 見矩陣 Rename 欄） | `gnx rename --symbol old --new-name new --dry-run` 再去掉 `--dry-run` |
| 列出本機所有已索引的 repo | `gnx list` |
| 重新註冊一個搬過家的 `.gitnexus-rs/` | `gnx index <path>` |
| 完全刪除索引 | `gnx clean --repo <path>` |
| Multi-branch / multi-worktree 流程 | `gnx init`（裝 hook）、`gnx prune --branch X`、`gnx rename-branch --from A --to B` |
| 互動式設定精靈 | `gnx config` |
| 檢查磁碟上的圖譜是否過期 | `gnx status` |
| 列出某個 community/cluster 的成員 | `gnx cluster --id <n>` 或 `--name <anchor>` |
| 對照語言 oracle 驗證 resolver 判斷 | `gnx verify-resolver --oracle … --gnx … --lang <ts\|py\|rs>` |

### 命令參考

所有命令預設從當前目錄讀取 `.gitnexus-rs/graph.bin`，可用 `--graph <path>` 覆寫。讀取端命令用 `--repo <name-or-path>` 在多 repo 註冊表中指定目標。

#### 索引生命週期

| 命令 | 用途 | 重點 flag |
|---|---|---|
| `analyze --repo <path>` | 建立 / 刷新 `<path>` 的圖譜。預設增量（內容雜湊快取）。 | `--embeddings`（建 BGE-M3 向量）· `--drop-embeddings` · `--force`（強制全量重建） · `--dump-resolver <file>` |
| `analyze-here` | `analyze --repo .` 的便利包裝。 | 同上 + `--no-cache` |
| `init` | 安裝 git reference-transaction hook，分支切換自動追蹤索引。 | `--force` · `--no-chain` |
| `prune --branch <name> --repo <p>` | 刪除過時的 branch-scoped 索引目錄。 | — |
| `rename-branch --from <a> --to <b> --repo <p>` | 重命名 branch 索引目錄。 | — |
| `clean [--repo <p>] [--all]` | 刪除一個或全部的 `.gitnexus-rs/`。 | — |
| `index [<path>]` | repo 搬家後重新註冊 `.gitnexus-rs/`。 | — |
| `remove <target>` | 從註冊表移除一筆（依名稱 / 別名 / 路徑）。 | `--force`（保留） |
| `list` | 列出本機所有已索引的 repo。 | `--format text\|json\|toon` |
| `status` | 單一 repo 的新舊度檢查（圖譜 vs 工作目錄）。 | `--repo <p>` |
| `config` | 互動式 TOML 編輯精靈（`.gitnexus-rs/config.toml`）。 | `--repo <p>` |

#### 圖譜查詢

| 命令 | 用途 | 重點 flag |
|---|---|---|
| `query --query <text>` | BM25（含可選語意）符號搜尋。 | `--format` |
| `context --name <sym>` / `--uid <UID>` | 單一符號 → metadata、裝飾器、簽名、呼叫者、被呼叫者。 | `--kind` · `--file_path` · `--relation_types` · `--include_tests` |
| `impact --target <sym> --direction <dir>` | 爆炸半徑 / 依賴 traversal。`dir` ∈ `upstream`（誰呼叫 X）、`downstream`（X 呼叫了什麼）。 | `--depth <n>`（預設 5） · `--high-trust-only` · `--min-confidence <f>` · `--include-tests` · `--kind` · `--file_path` |
| `cypher '<query>'` | 任意 openCypher 模式匹配。`m.content` 回傳原始碼。 | `--format` |
| `process --name <name>` | 單一執行流的步驟 trace。 | — |
| `cluster --id <n>` / `--name <anchor>` | 列出某個 community / cluster 的成員。 | — |

#### HTTP route 與 tool call

| 命令 | 用途 | 重點 flag |
|---|---|---|
| `route-map` | 列出所有萃取到的 HTTP route（含 declarative `@Get` 與 imperative `app.get()`）。 | — |
| `api-impact --route <path>` | route → handler → 上游呼叫者。 | `--method GET\|POST\|…` · `--depth <n>`（預設 3） |
| `tool-map` | 對已知 HTTP / DB / Redis / queue client 的呼叫。 | `--category http,db,redis,queue` |

#### 洞察與變更追蹤

| 命令 | 用途 | 重點 flag |
|---|---|---|
| `summarize` | Markdown / JSON 專案概覽：架構、熱門檔案（in-edge centrality）、重要符號。 | `--top-files <n>` · `--top-communities <n>` · `--top-symbols <n>` · `--include-orphans` · `--output <file>` |
| `doctor` | 框架覆蓋率 + 盲區目錄 + 圖譜狀態。LLM 契約報表。 | `--format compact\|json` |
| `detect-changes` | git diff 改到的符號 + 受影響的執行流。 | `--scope unstaged\|staged\|all\|compare` · `--base-ref <ref>`（搭 `compare` 必填） · `--kind` · `--include-tests` · `--high-trust-only` |

#### 重構

| 命令 | 用途 | 重點 flag |
|---|---|---|
| `rename --symbol <old> --new-name <new>` | AST 驅動的跨檔 rename（14 種語言：Python、TS/TSX、JS、Rust、Java、Kotlin、C#、Go、PHP、Ruby、Swift、C、C++、Dart）。請務必先跑 `--dry-run`。 | `--dry-run` |

#### 診斷

| 命令 | 用途 | 重點 flag |
|---|---|---|
| `verify-resolver --oracle <f> --gnx <f> --lang <ts\|py\|rs>` | 對照語言 oracle 驗證 resolver dump。給 parity harness 用。 | `--report <md-path>` |

> 每個命令的完整 flag 可用 `gnx <command> --help` 確認。CLI 採非互動式設計（LLM 友善）：所有 flag 透過 `--help` 暴露，所有輸出走 stdout 且可解析。

## 語言矩陣

graph-nexus 自己的「31 種語言 × 各維度實作狀態」清單。每個 cell 回答一個問題：**這個語言、這個維度，我們今天有沒有做？**

這份矩陣**不是**用來跟任何其他工具對 parity。我們從 GitNexus 的 9 維度切分得到設計啟發（致敬段在最上方），但每個 cell 描述的是**自己的實作狀態**，對照的是自己的 roadmap，不是任何外部宣稱。

**圖例**：
- ✓ &nbsp;**已實作** — 這個語言這個維度我們現在有抽
- ☐ &nbsp;**語言適用、還沒做** — 該語言有此概念、我們可以擴；當作 roadmap 標記
- — &nbsp;**不適用** — 該語言根本沒這個概念（例如 Dockerfile 沒有 Frameworks）

> 分隔線以下的 17 列，`—` 暫時把「不適用」與「未實作」合併 — 還沒做 per-cell audit。**只有 Rename 欄是例外**：底部 17 個全部 `☐`，因為每種語言都有 identifier 可以 rename。其餘 cell 細分 `☐`/`—` 排為後續 audit。

| 語言 | Imports | Named | Exports | Heritage | Types | Ctor | Config | Frameworks | Entry | Call | Rename |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| TypeScript | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| JavaScript | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Python | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Java | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Kotlin | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| C# | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ☐ | ✓ | ✓ | ✓ |
| Go | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Rust | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| PHP | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Ruby | ✓ | — | ✓ | ✓ | — | ✓ | ✓ | ☐ | ✓ | ✓ | ✓ |
| Swift | ✓ | — | ✓ | ✓ | ☐ | ✓ | ✓ | ☐ | ✓ | ✓ | ✓ |
| C | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ☐ | ✓ | ✓ | ✓ |
| C++ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ☐ | ✓ | ✓ | ✓ |
| Dart | ✓ | ✓ | ✓ | ✓ | ☐ | ✓ | ✓ | ☐ | ✓ | ✓ | ✓ |
| ─── *以下結構解析為主，per-cell audit 待補* ─── | | | | | | | | | | | |
| Bash | ✓ | — | — | — | — | — | — | — | — | ✓ | ☐ |
| Lua | ✓ | — | — | ✓ | — | — | — | — | — | ✓ | ☐ |
| Solidity | ✓ | — | ✓ | ✓ | — | — | — | — | — | ✓ | ☐ |
| Crystal | ✓ | — | — | ✓ | — | — | — | — | — | ✓ | ☐ |
| Nim | ✓ | — | — | ✓ | — | — | — | — | — | ✓ | ☐ |
| Cairo | ✓ | — | — | — | — | — | — | — | — | ✓ | ☐ |
| Move | ✓ | — | — | — | — | — | — | — | — | ✓ | ☐ |
| Zig | ✓ | — | — | — | — | — | — | — | — | ✓ | ☐ |
| HCL | ✓ | — | — | — | — | — | ✓ | — | — | ✓ | ☐ |
| SQL | — | — | — | ☐ | — | — | — | — | — | ✓ | ☐ |
| Verilog | ✓ | — | — | — | — | — | — | — | — | ✓ | ☐ |
| Vyper | ✓ | — | — | — | — | — | — | — | — | ✓ | ☐ |
| Markdown | — | — | — | — | — | — | — | — | — | — | ☐ |
| GitHub Actions | ☐ | — | — | — | — | — | ✓ | — | — | — | ☐ |
| Docker Compose | — | — | — | — | — | — | ✓ | — | — | — | ☐ |
| Dockerfile | ✓ | — | — | — | — | — | ✓ | — | — | — | ☐ |
| YAML | — | — | — | — | — | — | ✓ | — | — | — | ☐ |

**per-cell 註腳**（cell 形狀需要解釋的）：
Bash Imports 是 `source`/`.`；Lua Imports 是 `require` + binding alias；Lua Heritage 是 `setmetatable(...,{__index=Parent})` 啟發式；Solidity Heritage 是 `is X, Y, Z`；SQL Heritage ☐ 是 foreign-key references 規劃中、未實作；GitHub Actions Imports ☐ 是 `uses:`（workflow → action）邊規劃中；Dockerfile Imports 是 `FROM <base>`。

**Roadmap（☐ 的 cells）** — 明確標「可做、未做」的：
- 6 個語言的 **Frameworks**（C# / Ruby / Swift / C / C++ / Dart）— 全新工作，無 reference 實作可抄。
- Swift / Dart 的 **Types** — grammar shape 差異夠大，先前用在 Go/C/C++ 的 dispatch 邏輯沒收斂；排入專項追蹤。

**近期完成**（脈絡備查）：
- 跨語言 Constructor Inference（14 種），以 Python `4e4fb1b` receiver-type binding 為原型。
- Java static import named bindings、C# `csproj` / `global.json` config、Go/Ruby/C/Dart 各自慣例的 Exports、跨語言 Entry Point scorer（整合 routes + `main()` + framework 裝飾器）。
- Wave 2（PR [#2](https://github.com/coseto6125/graph-nexus/pull/2)）：Go/C/C++ 的 Types（參數 / 回傳值 / 欄位 / 變數的宣告類型）、PHP（`composer.json`）+ Swift（`Package.swift`）的 Config、JS（Express + Hapi）/ Kotlin（Ktor）/ Go（gin + echo）/ PHP（Laravel）的 Frameworks。
- Matrix-opt batch（HEAD `86e65a7`）：Go 每個 struct field visibility、Dart underscore 慣例、Ruby `attr_*` metaprogramming + mixin 追蹤、TS/JS re-export alias 保留；額外 17 種裡，Bash 增 `source`/`.` imports、Lua 增 `require` aliases + metatable inheritance + table-assigned methods、Solidity 增 state-variable visibility。詳見 `docs/specs/2026-05-15-matrix-optimization-opportunities.md`。

### Call 偵測設計

Call 偵測集中在 `crates/graph-nexus-analyzer/src/calls.rs`。熱路徑 helper 是 `extract_calls(root, source, nodes, call_kinds)`：

- 每個語言 parser 傳入該 grammar 中代表 call 的 tree-sitter node kind — 例如 JS/TS 的 `["call_expression"]`、Lua 的 `["function_call"]`、Python 的 `["call"]`。
- Walker 對 grammar 無感：一次走完 AST、收集所有 call site、用 `callee_name_from(node, source)` 拿 callee 文字、用 `attach_to_enclosing(line, callee, nodes)`（最小 span 包覆）把每個 call 掛到對應的 enclosing `Function` / `Method`。
- OO 語言額外綁 **receiver type**（`obj.method` → 知道 `obj` 是什麼）。每個語言有自己的 receiver-type 模組（`<lang>/receiver_types.rs`）追蹤 local 變數標註與 class-scope `this`/`self`。Receiver type 存在 RawCall 上，讓下游 resolution 在 method 名稱衝突時挑出正確的 overload。
- Reflection / dynamic dispatch（`getattr(self, name)()`、JS dynamic `obj[k]()` 等）**不會**被推測性 resolve；會落到 `BlindSpot` record（遵循專案「老實的 unknown 勝於捏造邊」原則）。
- Call edges（`RelType::Calls`）是圖中最大的單一邊類型；`calls.rs` 的 saturating-conversion helper `safe_row` 防止 row 超過 `u32::MAX` 損壞 call-to-function 對應關係。

## 🏗️ 系統架構

```
crates/
├── graph-nexus-core        # 零拷貝圖譜定義 (rkyv)、增量快取演算法、圖譜檢索 helper
├── graph-nexus-analyzer    # Tree-sitter 解析器、BGE-M3 向量生成、HTTP 路由偵測器
└── graph-nexus-cli         # `gnx` 命令列、Tantivy BM25 全文引擎、Token 最佳化輸出
```

解析器 (Analyzer) 透過 MPSC 通道將 AST 節點傳遞給單一的 Builder 執行緒。Builder 負責組裝圖譜、推導 API 路由與文件分類，最後將其序列化為零拷貝的 `.gitnexus-rs/graph.bin`。讀取端（如 `context` 或 `query`）透過 mmap 直接映射硬碟檔案，達成零延遲查詢。

## ⚙️ 調校

| 環境變數 | 預設值 | 作用 |
|---|---|---|
| `GNX_MAX_FILE_BYTES` | `16777216` (16 MiB) | 解析時跳過超過此大小的原始碼檔案，將 worker 最壞情況 RAM 控制在 `num_threads × MAX`。索引含產生器/編譯輸出時可調高；記憶體受限機器可調低。 |
| `GNX_EMBED_BATCH` | `32` | fastembed 推論 batch 大小。調低可降低 embedding 階段尖峰駐留（BGE-M3 INT8 下 16 ≈ 200 MiB、32 ≈ 300 MiB）。 |
| `GNX_CSPROJ_MAX_DEPTH` | `4` | `*.csproj` 掃描遞迴深度。深層 .NET monorepo 可調高。 |
| `GNX_MODEL_CACHE` | `$HF_HUB_CACHE` ⤳ `$HF_HOME/hub` ⤳ `~/.cache/huggingface/hub` | 覆寫 BGE-M3 模型快取目錄。 |

## 📄 授權條款

基於 [PolyForm Noncommercial 1.0.0](./LICENSE) 授權。明確允許個人使用、學術研究、業餘專案與非營利組織。

**本授權不允許商業使用。** 如需商業授權，請聯繫上游 GitNexus 原作者 Abhigyan Patwari。
