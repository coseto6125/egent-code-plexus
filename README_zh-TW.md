# EgentCodePlexus

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)
[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)

給 **LLM 與 AI 程式碼代理（AI code agents）** 用的代碼智能圖譜 — 單次 CLI 調用、mmap 零拷貝、每次查詢亞秒級。

[English README](./README.md)

---

## 🎯 核心使命

`ecp` 的存在是為了成為自主 AI 代理在每個任務中調用 20-50 次的結構化知識層。所有的設計決策都源於這個前提：

- **為代理而建，非為 IDE。** 輸出格式節省 Token（TOON / 精簡 JSON），每個旗標都透過 `--help` 顯露，每個命令都是非互動式且可解析的。沒有 UI，沒有消耗代理上下文視窗的人類閱讀排版。
- **無預熱，無守護進程。** 每次調用都會 `mmap` 一個零拷貝的 `rkyv` 圖譜檔案並退出。讀取查詢在 **~140–170 ms** 內返回（*包含進程啟動*）；2.2 萬個檔案的專案冷啟動索引低於 3 秒。代理可以在不考慮伺服器啟動成本的情況下，每個任務發起數十次查詢，且沒有「伺服器當機，請重啟」的故障模式。
- **老實的回答勝於可讀的圖表。** 當呼叫點無法靜態解析（動態派發、未解析的導入、反射）時，`ecp` 會記錄 `BlindSpot`，而不是隨便連一條邊。一個基於幻覺依賴行動的代理，其成本遠高於一個獲得「我不知道」並能繞道而行的代理。
- **廣泛的語言覆蓋。** 在結構層級解析 31 種語言，讓現代多語言專案（服務代碼 + Dockerfile + GitHub Actions + Terraform + SQL + 智能合約）在離開主語言後不再是黑洞。

🎙️ **[Agent 訪談紀錄](./interviews/README_zh-TW.md)** — 查看真實 AI Agent (Gemini CLI, Codex) 在自主工作流中如何使用與評價 `ecp`。

致敬 [GitNexus](https://github.com/abhigyanpatwari/GitNexus)（原作：[Abhigyan Patwari](https://github.com/abhigyanpatwari)）— 同樣的核心想法（repo 的結構化知識圖譜），用 Rust 重寫成面向**另一群受眾**的版本。基於 [PolyForm Noncommercial 1.0.0](./LICENSE) 授權；完整第三方歸屬清單請見 [NOTICES.md](./NOTICES.md)。

---

## ⚡ 效能表現

上述的使命說明了 `ecp` 為何如此構建。本節則是實測數據。

### 與上游 GitNexus 的正面對決

在 [gitnexus](https://github.com/abhigyanpatwari/GitNexus) 代碼庫（TypeScript）上使用 `scripts/parity/benchmark_vs_gitnexus.py` 測量：

| 階段 | ecp (Rust) | gitnexus (Node) | 加速倍率 |
|---|---|---|---|
| **冷啟動索引** | **~970 ms** | ~58 s | **60×** |
| **符號上下文** | **~70 ms** | ~430 ms | **6×** |
| **影響範圍** | **~70 ms** | ~460 ms | **6×** |
| **Cypher 查詢** | **~70 ms** | ~400 ms | **5×** |

*註：`ecp` 查詢延遲包含了完整的進程啟動時間（無背景常駐程式）。GitNexus (v1.6.5) 的延遲是在已索引且預熱的情況下透過 CLI 測量。*

### 可擴展性 — `.sample_repo` 單次運行（包含 25+ 種語言、約 40 個真實開源專案、總計 2.1 GB 的多語言測試集，用於跨語言壓力測試）

**攝入效能：**

| 階段 | 數值 |
|---|---|
| 已索引檔案數 | **22,645** 個，跨 25 種偵測到的語言 |
| 冷啟動耗時 | **2.60 s** (解析 + 解析 + 序列化) |
| 增量索引耗時 | **4.9 ms** (xxh3_64 雜湊掃描，零變動檔案) |
| 測試硬體 | AMD Ryzen 9 9950X (16 邏輯核心), 39.2 GiB RAM, Linux 6.6.87 |

**單次查詢延遲（包含進程啟動）：**

| 查詢 | 中位數 | 備註 |
|---|---|---|
| `coverage` (註冊表總覽) | **1.4 ms** | 最小讀取 — 僅 mmap 註冊表 |
| `routes` (全專案 HTTP 路由圖譜) | **142.3 ms** | 列舉聲明式 + 指令式定義 |
| `coverage --detailed` (框架 + 盲區) | **143.4 ms** | 完整註冊表 + 各框架評分 |
| `impact <symbol> --direction down` | **145.0 ms** | 遍歷 Calls / Extends 邊 (BFS) |
| `inspect <symbol>` (簽名 + 呼叫鏈) | **145.6 ms** | 符號解析 + 1-hop 遍歷 |
| `find <name> --mode bm25` (詞法搜尋) | **154.5 ms** | Tantivy 查詢 + 5 個儲存桶分區 |
| `cypher 'MATCH (a:Class)-[:HasMethod]->(b:Method) ...'` | **161.5 ms** | 單一模式，回傳單列 |
| `cypher 'MATCH (a:Method)-[:Calls]->(b:Method) ...'` | **174.2 ms** | 較廣泛模式，匹配較多結果 |
| `impact --baseline HEAD~1` (變更爆炸半徑) | **359.0 ms** | git diff + 平行單檔解析 + BFS |

重現方式：`python scripts/benchmark_ecp.py`。

---

## 跟上游 GitNexus 的差別

> **不是 drop-in 替代品。** 上游是為人類設計的 Agent 平台；egent-code-plexus 是為 **Coding AI Agent** 量身打造的結構化知識層 — 不同的受眾、不同的權衡。

| 維度 | egent-code-plexus | GitNexus |
|---|---|---|
| **核心受眾** | **Coding AI Agent** | 人類開發者 + IDE 整合 |
| **運行模式** | 無狀態 One-shot CLI (零預熱) | 長駐 MCP server |
| **效能表現** | **< 2.5s 冷啟動 / < 150ms 查詢** | ~60s 冷啟動 / ~400ms 查詢 |
| **未解析的邊** | `BlindSpot` 記錄 (老實的未知) | 啟發式猜測 |
| **預設輸出** | TOON / 精簡 JSON (省 Token) | Wiki / UI 渲染 |
| **支援語言** | 31 (14 種深度 + 17 種結構層級) | 14 (深度 9 維度覆蓋) |
| **儲存層** | Rust + `rkyv` 零拷貝 mmap | Node.js + LadybugDB |

**8 個維度的完整細節、哲學與決策矩陣 → [docs/vs-gitnexus.md](./docs/vs-gitnexus.md)**

---

## 📦 安裝

在第一個帶 tag 的 Release 發佈前，安裝腳本會 fallback 到 cargo source build。等 release assets 存在後，預編譯 binary 會成為最快路徑。

```bash
# Linux / macOS (最短路徑；尚無 Release assets 時需要 cargo/rustup)
curl -sSfL https://raw.githubusercontent.com/coseto6125/egent-code-plexus/main/install.sh | sh

# Windows PowerShell
iwr https://raw.githubusercontent.com/coseto6125/egent-code-plexus/main/install.ps1 -UseBasicParsing | iex

# 明確使用 cargo (同樣是 source build，不經 installer wrapper)
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

可選的 CPU 最佳化 source build：

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

---

## 🚀 快速上手

```bash
# 1. 為當前目錄建立索引 (增量式；第一次查詢也會自動觸發)
ecp admin index --repo .

# 2. 定位符號 — 預設為精準名稱比對
ecp find loginUser
ecp find login --mode bm25       # BM25 排序，分為 source/tests/ref/doc/config 等 bucket

# 3. 爆炸半徑 — 我改這裡會壞掉什麼？
ecp impact validateUser --direction upstream

# 4. 完整的符號上下文 (簽名、body、呼叫者、被呼叫者、1-hop 影響)
ecp inspect validateUser

# 5. 專案中所有的 HTTP 路由 (聲明式 @Get + 指令式 app.get())
ecp routes
ecp routes /api/users --method POST     # 路由 → 處理器 → 呼叫鏈
```

讀取端命令接受 `--format text|json|toon`。預設為該命令最省 Token 的格式（多數為 `toon`；`find` 預設為 `text`；`cypher`/`coverage` 預設為 `json`）。

---

## CLI 命令概覽

雙層結構 — 頂層為 **agent 命令** (query/refactor/verify)，以及 `ecp admin` 下的 **admin 命令** (registry/hooks/破壞性操作)。詳見 `ecp --help` 與 `ecp admin --help`。

| 命令 | 用途 |
|---|---|
| `inspect <name>` | 單一符號 → metadata、裝飾器、簽名、呼叫者、被呼叫者、1-hop 影響 |
| `find <pattern>` | 定位符號 — 精準 (預設) · `--mode fuzzy` 子字串 · `--mode bm25` 詞法排序 |
| `impact <name> --direction <up\|down>` | 帶信心度過濾的爆炸半徑 traversal。`--since <ref>` 用於變更集影響分析。 |
| `rename --symbol <old> --new-name <new>` | AST 感知的跨檔重命名 (14 種語言)。務必先執行 `--dry-run`。 |
| `cypher '<query>'` | openCypher 逃生艙；`m.content` 返回原始碼。 |
| `coverage` | Registry 總覽、框架覆蓋率、盲區目錄、圖譜新鮮度。 |
| `routes [<path>]` | 列出 HTTP 路由；帶 `<path>` 時顯示處理器 + 呼叫者。 |
| `contracts` | 跨 repo 的 API 合約清單 (routes / queue / RPC)。 |
| `diff` | 解析器 Delta — 邊界綁定層級降級 + 路由 / 合約變更。 |
| `tool-map` | 透過分析導入綁定，列出對外部 HTTP / DB / Redis / queue client 的呼叫。 |
| `shape-check` | HTTP 消費者訪問模式與路由響應形狀之間的偏移。 |
| `peers` | 多會話協作 (status / diff / log / gc)。 |
| `review` | 聚合式稽核：一次執行 impact + coverage + tool-map + shape-check + diff。 |

Admin 命名空間 (`ecp admin <cmd>` — 隱藏於頂層說明)：

| 命令 | 用途 |
|---|---|
| `index --repo <path>` | 建立 / 刷新圖譜；透過 xxh3_64 內容快取達成增量。`--force` 為全量。 |
| `drop / prune / rename-branch` | 索引生命週期：刪除、清理過時分支目錄、重命名分支。 |
| `install-hook` | 安裝 git reference-transaction hook (自動追蹤分支切換)。 |
| `config` | `.ecp/config.toml` 互動式精靈。 |
| `mcp serve` / `mcp tools` | 給 LLM host 用的 MCP server (stdio)。 |

不帶子命令執行 `ecp admin` 會開啟互動式 admin TUI，用於索引維護、host 整合、設定、群組與診斷。

---

## MCP 伺服器

`ecp` 內建 MCP 伺服器，將核心命令暴露為 MCP 工具。支援 MCP 的主機 (Claude Code, Cursor, Windsurf, Cline, Codex CLI, Gemini CLI) 可以註冊 `ecp` 並自主調用。

```bash
ecp admin mcp tools          # 檢視暴露的工具表面
ecp admin mcp serve          # 啟動伺服器 (預設：spawn 模式，每次調用啟動新進程)
```

給人操作的漸進式路徑：

```text
ecp admin
→ Agent Integrations
→ MCP
→ <host>
→ install
```

## Codex CLI native 整合

Codex native 路徑與 MCP 分離。它會為 `openai/codex` fork 準備 patch，不會直接修改正在執行的 Codex 安裝：

給人操作的漸進式路徑：

```text
ecp admin
→ Agent Integrations
→ Codex CLI
→ install
→ native-tools
```

內建 skills 使用相同的漸進式路徑：

```text
ecp admin
→ Agent Integrations
→ Codex CLI
→ install
→ skills
→ all | ecp | simplify
```

給 AI agent 與自動化流程使用的指令化路徑：

```bash
ecp admin codex install native-tools
ecp admin codex install skills all
ecp admin codex install skills ecp
ecp admin codex install skills simplify
```

內建 skills 用來教 agent 判斷 help 本身無法推導的使用場景：

| Skill | 適用場景 |
|---|---|
| `ecp` | agent 需要判斷何時用圖譜感知的 symbol、impact、route、contract、rename 流程，而不是 grep / 讀檔。 |
| `simplify` | agent 在 review changed code 時，應先看 ecp impact、盲區、egress、shape drift、resolver delta，再讀 raw diff。 |

`native-tools` 元件會寫出：

```text
~/.config/ecp/host-integration/codex-cli.patch
```

在 Codex CLI fork 內套用 patch，接著把產生的 module 接進 Codex 的 tool registry：

```bash
cd /path/to/openai-codex-fork
git apply ~/.config/ecp/host-integration/codex-cli.patch
```

若要檢查已套用 native marker 的 fork，先設定 `ECP_CODEX_CLI_CHECKOUT`，再於 TUI 內查看狀態：

```bash
ECP_CODEX_CLI_CHECKOUT=/path/to/openai-codex-fork ecp admin
# Agent Integrations → Codex CLI → status
```

等價的指令化檢查：

```bash
ECP_CODEX_CLI_CHECKOUT=/path/to/openai-codex-fork ecp admin codex status
ecp admin codex uninstall native-tools
ecp admin codex uninstall skills all
```

---

## 系統架構

```
crates/
├── ecp-core        # 零拷貝圖譜 (rkyv + mmap)、增量快取、圖譜查詢
├── ecp-analyzer    # Tree-sitter 解析器、HTTP 路由偵測、框架信心度
├── ecp-mcp         # MCP 伺服器 (stdio) — 將核心命令暴露為工具
└── ecp-cli         # `ecp` 執行檔、Tantivy BM25 引擎、Token 最佳化輸出
```

---

## 語言覆蓋範圍

在結構層級解析 31 種語言。其中 14 種（原 GitNexus 集合）獲得完整的 9 維度覆蓋。其餘 17 種為結構層級解析。

📊 **[完整語言能力矩陣](./docs/language-matrix.md)** — 各語言實作狀態與設計考量的詳細說明。

---

## 📄 授權與致謝

基於 [PolyForm Noncommercial 1.0.0](./LICENSE) 授權。

技術底層：
- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — 原始設計、CLI 介面與概念模型
- [tree-sitter](https://tree-sitter.github.io/) — 強大的增量 AST 解析
- [rkyv](https://rkyv.org/) — 零拷貝序列化框架 (Zero-copy deserialization)
- [Tantivy](https://github.com/quickwit-oss/tantivy) — 高效 Rust 全文搜尋引擎 (BM25)
- [Rayon](https://github.com/rayon-rs/rayon) — 用於多核心並行 AST 解析的數據並行庫
- [xxhash (xxh3_64)](https://xxhash.com/) — 極速非加密雜湊，用於增量索引的內容校驗
- [DashMap](https://github.com/xacrimon/dashmap) — 高效能並行雜湊表，用於圖譜組裝
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — 零拷貝記憶體映射，實現亞毫秒級圖譜讀取
- [msgspec](https://github.com/jcrist/msgspec) — 高效能 JSON 序列化，用於進程間通訊 (IPC)

## 發佈狀態

目前已驗證的安裝路徑是 `cargo install --git ...`，也就是從原始碼建置 `ecp`。release installer 已包含 checksum 與 provenance verification 流程，但必須等 tag 與 release assets 發佈後，binary 下載路徑才能做端到端驗證。Agent 安裝引導文件位於 [docs/skills/ecp-onboard/ONBOARDING.md](./docs/skills/ecp-onboard/ONBOARDING.md)；它用來引導使用者完成安裝、首次索引、可選群組、MCP wiring 與後續建議。輔助式配置與設定流程仍在完善中。
