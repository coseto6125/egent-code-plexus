<div align="center">

# `ecp` · EgentCodePlexus

### 為 AI 代理而生、而非為人類打造的結構化代碼圖譜。

*2.2 萬個檔案 2.6 秒完成索引 · 任何查詢 &lt;175 ms 內回應 · 誠實的未知，絕不捏造邊。*

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)
![Cold index 2.6s](https://img.shields.io/badge/cold_index-2.6s%20%2F%2022k%20files-brightgreen)
![Query latency](https://img.shields.io/badge/query-%3C175ms%20cold-blue)
![Languages](https://img.shields.io/badge/languages-31%20parsed-orange)
![License](https://img.shields.io/badge/license-PolyForm%20NC-lightgrey)
![Built with Rust](https://img.shields.io/badge/built_with-Rust-orange?logo=rust)
![Status early release](https://img.shields.io/badge/status-early%20release-yellow)

[English](../../README.md) · **繁體中文** · [简体中文](./README_zh-CN.md) · [日本語](./README_ja.md) · [한국어](./README_ko.md) · [Español](./README_es.md) · [Português](./README_pt-BR.md) · [Русский](./README_ru.md) · [हिन्दी](./README_hi.md)

</div>

---

自主程式碼代理在**每個任務中發起 20–50 次結構化查詢**。這些查詢全都打在為人類打造的工具上：IDE 側邊欄、需要預熱的守護進程、為人眼閱讀而排版的輸出。這個錯配具體呈現為三種故障模式：

1. **Token 浪費** — `grep` 傾倒了 400 行，而代理只需要其中 10 個符號
2. **破壞性重構** — 解析器猜錯，漏掉一個呼叫者就此溜過
3. **幻覺依賴** — 當靜態分析無法觸及某條邊時，工具乾脆捏造一條

`ecp` 就是為了消除這三者而生。

| 故障模式 | `ecp` 的解法 |
|---|---|
| 原始搜尋輸出炸掉上下文視窗 | **TOON / 精簡 JSON** — 只給符號、行號與邊，毫無填充 |
| 漏掉呼叫者，下游無聲崩壞 | **`impact`** — 在真實的呼叫與繼承邊上計算精確的影響範圍 |
| 在代理推理中混入捏造的依賴 | **`BlindSpot` 紀錄** — 帶型別、可繞道的誠實未知 |
| 一離開主語言圖譜就變黑洞 | **31 種語言** — 服務代碼、IaC、SQL、智能合約一次走訪全覆蓋 |

---

## 🎯 設計原則

每個設計決策都源於同一個問題：*接收方代理究竟需要什麼？*

**輸出是資料結構。** TOON 與精簡 JSON 只攜帶代理做下一步決策所需的內容。沒有散文摘要、沒有視覺裝飾、沒有吃掉上下文預算的章節標題。各命令的格式預設值，對多數 LLM prompt 而言已經是正確選擇。

**無狀態、零預熱。** 每次調用都 `mmap` 一個零拷貝的 `rkyv` 圖譜檔案後退出。**每次查詢 ~140–170 ms，已含啟動時間。** 沒有要維持存活的守護進程、沒有預熱階段、沒有「伺服器當機請重啟」的復原路徑。代理可以在不付出進程啟動成本的前提下，每個任務發起 50 次查詢。

**寧可 BlindSpot，也不要幻覺。** 當 `ecp` 無法靜態解析某個呼叫點時——動態派發、反射、未解析的導入——它會發出一筆 `BlindSpot` 紀錄：一個帶名稱、帶型別、明確標示的圖譜缺口。代理能繞過已知的未知，卻無法從一個自信的捏造中復原。

**預設多語言。** 31 種語言的結構深度解析。服務代碼、Dockerfile、GitHub Actions、Terraform、SQL、Move、Solidity——一次走訪即覆蓋所有層。不必切換語言，也就不會出現圖譜盲區。

🎙️ **[Agent 訪談紀錄](../../interviews/README.md)** — Gemini CLI 與 Codex 描述它們在實際自主任務流中如何使用 `ecp`。

致敬 [GitNexus](https://github.com/abhigyanpatwari/GitNexus)（原作 [Abhigyan Patwari](https://github.com/abhigyanpatwari)）——同樣的結構化圖譜概念，用 Rust 重寫，面向不同受眾。授權 [PolyForm Noncommercial 1.0.0](../../LICENSE.md)；必要的歸屬清單見 [NOTICES.md](../../LICENSES/NOTICES.md)。

---

## ⚡ 實測數據

三方實測對決：[`codegraph`](https://github.com/colbymchenry/codegraph)（Node + SQLite）與上游 [`gitnexus`](https://github.com/abhigyanpatwari/GitNexus)（Node）——相同 checkout、相同機器。`ecp` 是無狀態一次性 CLI：以下所有延遲**皆含完整進程啟動**，無守護進程、無預熱。

*版本：`ecp` 0.4.2 · `codegraph` 0.9.4 · `gitnexus` 1.6.5。所有工具在可設定時均以 1 MiB 最大檔案大小為上限（`gitnexus` 硬編碼 512 KB）。`ecp` 取 5–7 次執行中位數。硬體：AMD Ryzen 9 9950X（16 邏輯核心）、Linux。*

### `microsoft/vscode` — 14,874 個檔案、密集單語言 TypeScript

| 指標 | **`ecp`** | `codegraph` | `gitnexus` |
|---|---|---|---|
| **冷啟動索引** | **4.6 s** | 166.9 s | **DNF** — 27 分鐘後強制終止 |
| 記憶體峰值 RSS | **~1.0 GiB** | 1.7 GiB | 4.6 GiB（仍在攀升） |
| 符號查找 / 查詢 | **34.6 ms** | 169.5 ms | — |
| 呼叫者 / 影響範圍 | **27.2 ms** | 172.4 ms | — |
| 檢視 / 上下文 | **35.0 ms** | 415.9 ms | — |
| 影響基準（git-diff） | **725.9 ms** | N/A — 無此模式 | — |
| 圖節點數 | **507,257** | 315,498 | — |
| 圖邊數 | 916,380 | **986,709** | — |
| 磁碟索引大小 | **87 MiB** | 671 MiB | — |
| 已索引檔案數 | **14,874** | 10,814 | — |

*`gitnexus` 未完成——在記憶體內圖解析階段卡住 27 分鐘後強制終止（RSS 4.6 GiB，無輸出寫入）。*

### `abhigyanpatwari/GitNexus` — 3,232 個檔案、多語言（三者均能完成的語料）

| 指標 | **`ecp`** | `codegraph` | `gitnexus` |
|---|---|---|---|
| **冷啟動索引** | **0.74 s** | 11.2 s | 77.6 s |
| 記憶體峰值 RSS | **264 MiB** | 501 MiB | 2.5 GiB |
| 查找 / 查詢 | **9.4 ms** | 103.5 ms | — |
| 呼叫者 / 影響範圍 | **9.2 ms** | 104.2 ms | 297.6 ms |
| 檢視 / 上下文 | **9.4 ms** | — | 295.5 ms |
| 圖節點數 | **49,122** | 19,604 | 30,223 |
| 圖邊數 | **48,271** | 39,155 | 47,218 |
| 磁碟索引大小 | **7.7 MiB** | 37 MiB | 306 MiB |
| 已索引檔案數 | **3,232** | 2,968 | 3,232 |

**冷啟動索引：比 `codegraph` 快 15–37×；`gitnexus` 在真實大型 repo 上無法完成。記憶體最低、磁碟索引最小、圖最密——在各種規模下皆如此。**

### 規模：`.sample_repo` — 22,645 個檔案、25 種語言、2.1 GB 多語言語料

**索引攝取：**

| 指標 | 數值 |
|---|---|
| 索引檔案數 | **22,645** 個，橫跨 25 種偵測到的語言 |
| 冷啟動攝取 | **2.60 s**（解析 + 解析綁定 + 序列化） |
| 增量攝取 | **4.9 ms**（xxh3_64 雜湊走訪，零髒檔） |
| 硬體 | AMD Ryzen 9 9950X（16 邏輯核心）、39.2 GiB RAM、Linux 6.6.87 |

**每次查詢延遲，已含進程啟動：**

| 查詢 | 中位數 | 涵蓋內容 |
|---|---|---|
| `summary` | **1.4 ms** | registry mmap — 最小的讀取 |
| `routes` | **142.3 ms** | 宣告式 + 命令式路由列舉 |
| `summary --detailed` | **143.4 ms** | 完整 registry + 各框架信心評分 |
| `impact --direction down` | **145.0 ms** | 在 Calls / Extends 邊上做 BFS |
| `inspect` | **145.6 ms** | 符號解析 + 一跳走訪 |
| `find --mode bm25` | **154.5 ms** | Tantivy 查詢 + 5 桶分區 |
| `cypher`（窄查詢） | **161.5 ms** | 單一模式、單列結果 |
| `cypher`（寬查詢） | **174.2 ms** | 較寬模式、更多匹配 |
| `impact --baseline HEAD~1` | **359.0 ms** | git diff + 每檔平行解析 + BFS |

完整重現：`python scripts/benchmark/benchmark_ecp.py`。

### 與 Rust 同級競品的比較

`scripts/benchmark/benchmark_vs_competitors.py` 針對 [`codescope`](https://github.com/onur-gokyildiz-bhi/codescope)（SurrealDB 後端）與 `coraline`（SQLite 後端）橫跨 6 個階段測試：`cold-index`、`symbol-find`、`callers`、`file-context`、`route-map`、`cypher`。缺少的階段標為 `N/A`（缺席本身就是訊號）。結果會重新產生 `docs/benchmark-vs-competitors.md`。

```bash
python scripts/benchmark/benchmark_vs_competitors.py
python scripts/benchmark/benchmark_vs_competitors.py --corpus path/to/repo --iterations 5 --no-plot
```

---

## 🆚 對比上游 GitNexus

同樣的結構化圖譜概念，不同的受眾。並非即插即用的替代品——依「誰讀取輸出、用它做什麼」來選擇。

| 維度 | EgentCodePlexus | GitNexus |
|---|---|---|
| 主要使用者 | 自主 AI 程式碼代理 | 人類開發者 + IDE 整合 |
| 執行模型 | 無狀態單次 CLI（零預熱） | 長駐 MCP 伺服器 |
| 效能 | **< 2.5s 冷索引 / < 175ms 查詢** | ~60s 冷索引 / ~400ms 查詢 |
| 未解析的邊 | `BlindSpot` 紀錄（誠實的未知） | 啟發式猜測 |
| 預設輸出 | TOON / 精簡 JSON（省 Token） | Wiki / UI 渲染 |
| 語言 | 31（14 深度 + 17 結構） | 14（深度、9 維度） |
| 儲存 | Rust + `rkyv` 零拷貝 mmap | Node.js + LadybugDB |

**完整拆解、設計哲學與決策矩陣 → [docs/vs-gitnexus.md](../vs-gitnexus.md)**

---

## 📦 安裝

預編譯二進位檔隨每次 GitHub Release 發佈。當找不到對應的發佈資產時，安裝腳本才會回退到 cargo 原始碼建置。

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# 直接走 cargo（不經安裝腳本包裝）
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

CPU 調校的原始碼建置：

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

---

## 🚀 快速上手

無守護進程需啟動。無需設定。一個命令，從零到可查詢的圖譜。

```bash
# 索引（增量；若無索引，首次查詢會自動建立）
ecp admin index --repo .

# 找符號 — 預設精確匹配
ecp find loginUser
ecp find login --mode bm25            # BM25 排序，分成 5 個輸出桶

# 影響範圍 — 改這個會弄壞誰？
ecp impact validateUser --direction upstream

# 完整符號上下文（簽章、本體、呼叫者、被呼叫者、一跳影響）
ecp inspect validateUser

# HTTP 路由地圖（宣告式 @Get + 命令式 app.get()）
ecp routes
ecp routes /api/users --method POST   # 路由 → 處理器 → 呼叫鏈

# 檔案使用情況：誰讀 / 寫這個路徑？
ecp impact --literal session_meta.json
```

所有讀取端命令都接受 `--format text|json|toon`。各命令的預設值為最省 Token 的表示（多為 `toon`；`find` 預設 `text`；`cypher`/`summary` 預設 `json`）。

---

## 🛠️ CLI 命令面

兩層架構：頂層的**代理命令**（查詢 / 重構 / 驗證），以及 `ecp admin` 之下的**管理命令**（registry / hooks / 破壞性操作）。執行 `ecp --help` 與 `ecp admin --help` 查看完整旗標矩陣。

**代理命令：**

| 命令 | 用途 |
|---|---|
| `inspect <name>` | 符號 → 元資料、裝飾器、簽章、呼叫者、被呼叫者、一跳影響、所含方法 / 屬性 / enum 變體 |
| `find <pattern>` | 精確 · `--mode fuzzy` · `--mode bm25`（5 桶：source / tests / reference / document / config） |
| `find-schema-bindings <field>` | 跨類別 / 服務的 MirrorsField 啟發式邊 + blind-spot 候選 |
| `find-transaction-patterns [--class <Name>]` | Saga compensate/undo/rollback 名稱配對；≥0.75 → POSSIBLY_RELATED，<0.75 → BLIND_SPOT |
| `impact <name> --direction <up\|down>` | 帶信心過濾的影響範圍 BFS；`--since <ref>` 計算變更集影響 |
| `rename --symbol <old> --new-name <new>` | 跨 14 種語言、AST 感知的多檔重新命名。務必先 `--dry-run`。 |
| `cypher '<query>'` | openCypher 逃生口；`m.content` 回傳原始碼本體 |
| `summary` | Registry 總覽、框架覆蓋、LLM 可行動的 blind-spot 目錄、圖譜新鮮度 |
| `routes [<path>]` | HTTP 路由列舉（宣告式 + 命令式）；帶 `<path>` 時顯示處理器 + 呼叫鏈 |
| `contracts` | 跨 repo 的 API 契約清單（routes / queue / RPC） |
| `diff` | 解析器差異：綁定層級降級 + 路由 / 契約變更 |
| `tool-map` | 透過導入綁定分析找出外部 HTTP / DB / Redis / queue 的呼叫點 |
| `shape-check` | HTTP 消費者存取模式與 Route 回應結構之間的漂移 |
| `peers` | 多會話協作：`status / diff / say / inbox / log / thread / watch / gc` |
| `review` | 一次性審查：impact + summary + tool-map + shape-check + diff，只保留高信心訊號 |

**管理命令**（`ecp admin <cmd>`）：

| 命令 | 用途 |
|---|---|
| `index --repo <path>` | 建立 / 刷新圖譜；經由 xxh3_64 內容快取做增量。`--force` 完整重建。 |
| `drop / prune / rename-branch` | 索引生命週期：刪除、清理過時分支目錄、就地重新命名分支 |
| `install-hook` | Git reference-transaction hook（自動追蹤分支切換） |
| `config` | `.ecp/config.toml` 的互動式 TOML 精靈 |
| `mcp serve` / `mcp tools` | MCP 伺服器（stdio）；`tools` 列出對外暴露的工具面 |

除非提供 `--graph <path>`，所有命令都從 CWD 解析 `.ecp/graph.bin`。每個面向代理的命令都是非互動式的；每個輸出串流都可解析。

### 多會話夥伴同步

當多個 LLM 會話平行編輯同一個 repo 時，`ecp peers` 會揭露各會話的符號層級髒狀態，並支援會話間直接傳訊。透過 `ECP_SESSION_ID`、`CODEX_SESSION_ID`、`CODEX_THREAD_ID` 或 `CLAUDE_CODE_SESSION_ID` 註冊。

```bash
# 啟動 watcher（每會話一個；inbox 推送事件所必需）
ecp peers watch --start

# 現在還有誰在編輯？
ecp peers status                                  # text
ecp peers status --format json                    # {session_id, pid, watcher: alive|dead|not-started}

# 檢視某個夥伴的髒符號
ecp peers diff <peer-session-id> [<symbol>]

# 傳送訊息
ecp peers say "rebasing on main, hold pushes 5min"    # 廣播
ecp peers say --to <peer-session-id> "take auth.rs?"  # 定向

# 讀取與管理
ecp peers inbox
ecp peers log --limit 20
ecp peers thread <msg-id>

# 清理
ecp peers watch --stop && ecp peers gc
```

`watcher` 欄位區分 `alive` | `dead` | `not-started`——崩潰不會偽裝成「功能未被使用」。

### 可證明的程式碼審查裁決

`ecp review --verdicts` 從 `ecp diff` 的各區段預先計算出圖譜支撐的裁決。把 JSON 直接當作審查上下文傳入——免去 LLM 從原始 diff 重新推導呼叫者關係。

```bash
ecp review --since main --verdicts --format json
```

| 嚴重度 | 規則 |
|---|---|
| `RISK` | 存在跨檔呼叫者、移除了公開符號，或 diff 區域內有 blindspot |
| `WARN` | 只有檔內呼叫者，或路由被修改 |
| `INFO` | 找不到呼叫者，或新增了公開表面 |

裁決種類：`SIGNATURE_OR_BODY_CHANGED` · `NEW_PUBLIC_SURFACE` · `REMOVED_PUBLIC_SURFACE` · `ROUTE_CONTRACT_CHANGED` · `BLINDSPOT_IN_DIFF_REGION`

每筆裁決都會引用觸發它的確切 diff 區段與圖譜事實。完整規格：[docs/specs/2026-05-22-review-verdicts.md](../specs/2026-05-22-review-verdicts.md)。

---

## 🔌 代理整合

**有原生路徑時優先採用**——它會接上自動重建索引的 hooks 與工作流 skill，教代理*何時*值得為一次圖譜查詢付出往返成本。**MCP 是通用回退**，適用於任何會說該協定的 host。

| 代理 | 路徑 | 接上的能力 |
|---|---|---|
| Claude Code | 原生 | hooks + skills + 選用 MCP |
| Codex CLI | 原生 | skills（native-tools 尚待接線） |
| Gemini CLI | 原生 | 原生 skill **或** MCP |
| Cursor · Windsurf · Cline · Copilot · 任何 MCP host | MCP | MCP 伺服器 |

引導式設定：`ecp admin → Agent Integrations → <host>`。給自動化用的可腳本化路徑：`ecp admin <host> install <component>`。檢視任何 host：`ecp admin <host> status`。

### Claude Code

```bash
ecp admin claude install hooks          # settings.json：自動重建索引 + 上下文增強
ecp admin claude install skills all     # ecp + simplify skill 包（或：ecp | simplify）
ecp admin claude install mcp-server     # 選用 — hooks + skills + CLI 已足夠
```

Hooks 會在每次 Grep/Glob/Bash 時餵給圖譜上下文，無需明確的工具呼叫。`ecp` skill 教會 symbol / impact / route / contract / rename 工作流。`simplify` 驅動圖譜優先的程式碼審查。

### Gemini CLI

```bash
ecp admin gemini install native-skill   # 經 `gemini skills link` 連結
ecp admin gemini install mcp-server     # 經 `gemini mcp add` 註冊
```

`native-skill` 與 `mcp-server` 互斥——安裝其一會移除另一個。

### Codex CLI

```bash
ecp admin codex install skills all      # ecp + simplify；native-tools 待 Codex 接線
```

**工作流 skill：**

| Skill | 何時使用 |
|---|---|
| `ecp` | 代理需判斷在符號、呼叫者、路由、契約上，圖譜感知工作流是否勝過 grep / 讀檔 |
| `simplify` | 從 ecp impact、blind spots、egress、shape drift、解析器差異出發的程式碼審查 |

### MCP 回退（Cursor、Windsurf、Cline、任何 MCP host）

| Host | 設定檔 |
|---|---|
| Cursor | `~/.cursor/mcp.json` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` |
| Cline (VS Code) | `cline_mcp_settings.json`（MCP 面板 → "Edit MCP Settings"） |
| 通用 MCP host | 視 host 而定 |

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

```bash
ecp admin mcp tools    # 連線前先驗證暴露的工具面
ecp admin mcp serve    # 每次呼叫無狀態單次執行（零預熱成本）
```

---

## 🏗️ 架構

```
crates/
├── ecp-core        # 零拷貝圖譜（rkyv + mmap）、增量快取、圖譜查詢
├── ecp-analyzer    # Tree-sitter 解析器、HTTP 路由偵測器、框架信心評分
├── ecp-mcp         # MCP 伺服器（stdio）— 將核心命令暴露為工具
└── ecp-cli         # `ecp` 二進位檔、Tantivy BM25 引擎、Token 優化輸出
```

解析 → 解析綁定 → 序列化，全程透過一個 MPSC channel 匯入單一建構執行緒，由它組裝圖譜並寫出零拷貝的 `.ecp/graph.bin`。讀取路徑（`inspect`、`cypher`、`impact`…）直接 mmap 這個檔案——無反序列化步驟。xxh3_64 內容快取讓 2.2 萬檔 repo 的增量重建維持在亞秒級。

---

## 🌐 語言覆蓋

31 種語言的結構層級解析。**14 種完整深度**（TypeScript、JavaScript、Python、Java、Kotlin、C#、Go、Rust、PHP、Ruby、Swift、C、C++、Dart）——涵蓋導入、具名綁定、匯出、繼承、型別、建構子、設定、框架、進入點、呼叫與重新命名。**17 種僅結構**：Bash、Crystal、Cairo、Dockerfile、Docker Compose、GitHub Actions、HCL、Lua、Markdown、Move、Nim、Solidity、SQL、Verilog、Vyper、YAML、Zig。

📊 **[完整語言能力矩陣](../language-matrix.md)** — 各語言狀態與理由。

---

## ⚙️ 調校

| 環境變數 | 預設 | 效果 |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216`（16 MiB） | 攝取時跳過大於此大小的原始碼檔。將最壞情況下的 worker RAM 上限定在 `num_threads × MAX`。 |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | `*.csproj` 探索的目錄遞迴深度。深層巢狀的 .NET monorepo 可調高。 |

---

## 📜 授權與致謝

[PolyForm Noncommercial 1.0.0](../../LICENSE.md)。明確允許個人使用、研究、業餘專案，以及非營利組織。**本授權不授予商業使用權**——商業授權請聯繫上游 GitNexus 作者 Abhigyan Patwari。

構建於：
- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — 原始設計、CLI 命令面與概念模型
- [tree-sitter](https://tree-sitter.github.io/) — 穩健的增量 AST 解析
- [rkyv](https://rkyv.org/) — 零拷貝反序列化框架
- [Tantivy](https://github.com/quickwit-oss/tantivy) — 全文搜尋引擎
- [Rayon](https://github.com/rayon-rs/rayon) — 多核並行 AST 解析的資料平行性
- [xxhash (xxh3_64)](https://xxhash.com/) — 用於內容式增量索引的非密碼學雜湊
- [DashMap](https://github.com/xacrimon/dashmap) — 圖譜組裝用的並行雜湊表
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — 用於亞毫秒級圖譜存取的零拷貝記憶體映射
- [msgspec](https://github.com/jcrist/msgspec) — 行程間通訊用的高效能 JSON 序列化

代理上手（URL 引導、Claude Code skill、外掛安裝）：`docs/skills/ecp-onboard/`。並行不變式與重新驗證：`../../scripts/audit/audit-concurrency.sh`。

## 🚦 發佈狀態

已驗證的安裝路徑：`cargo install --git ...`，從原始碼建置 `ecp`。發佈用安裝腳本已內含 checksum 與來源驗證流程，但二進位下載路徑需要先發佈 tag 與 release 資產才能端到端驗證。面向代理的上手 skill：[docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md)。輔助式設定流程仍在打磨中。

---

<div align="center">

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)

</div>
