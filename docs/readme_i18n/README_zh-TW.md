# EgentCodePlexus

```
  ╔══════════════════════════════════════════════════╗
  ║  ecp                                             ║
  ║                                                  ║
  ║  structural code knowledge for AI agents         ║
  ║  one-shot cli  ·  zero-copy mmap  ·  ~140 ms     ║
  ╚══════════════════════════════════════════════════╝
```

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)

[![Linux](https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black)](https://github.com/coseto6125/egent-code-plexus/releases)
[![macOS](https://img.shields.io/badge/macOS-000000?style=for-the-badge&logo=apple&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/releases)
[![Windows](https://img.shields.io/badge/Windows-0078D6?style=for-the-badge&logo=windows&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/releases)
[![Claude Code](https://img.shields.io/badge/Claude_Code-D97757?style=for-the-badge&logo=anthropic&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/skill_sample/claude/SKILL.md)
[![Codex CLI](https://img.shields.io/badge/Codex_CLI-412991?style=for-the-badge&logo=openai&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/skill_sample/codex/ecp/SKILL.md)
[![Cursor](https://img.shields.io/badge/Cursor-000000?style=for-the-badge&logo=cursor&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/docs/skills/ecp-onboard/guides/04-mcp.md)

```
  cold index   ──  2.60 s   (60× upstream gitnexus)
  query p50    ──  142 ms   ( 6× upstream gitnexus)
  languages    ──  31       (14 deep + 17 structural)
  edge policy  ──  honest unknown, never hallucinated
```

[English](../../README.md) · **繁體中文** · [简体中文](./README_zh-CN.md) · [Español](./README_es.md) · [Русский](./README_ru.md) · [हिन्दी](./README_hi.md) · [日本語](./README_ja.md) · [한국어](./README_ko.md) · [Português (BR)](./README_pt-BR.md)

---

## ── 為什麼 ──

Code agent 一次任務裡會做 20–50 次代碼查詢。Grep 給你字串；自主代理需要的是符號、呼叫者、邊（edge），與「graph 推不出來時誠實說不知道」的訊號。

`ecp` 是這層結構知識，特性是：

- **無狀態。** 每次調用 `mmap` 一個 `rkyv` 零拷貝 graph，跑完直接 exit。沒有 daemon 要保溫、沒有「server 死了請重啟」這種失敗模式。
- **誠實。** 當呼叫點靜態解析不出來（動態派發、未解析 import、reflection），`ecp` 發出 `BlindSpot` 紀錄。代理對著幻覺出來的依賴下手，比代理收到一個「我不知道」然後繞道的成本高得多。
- **Token 便宜。** 預設輸出 TOON（緊湊 key:value）。每個 flag 都從 `--help` 出來、每個指令都 non-interactive 且 stdout 可解析。沒有耗 context window 的 UI 雜訊。
- **多語言。** 31 個語言做結構級解析 —— service code、Dockerfile、GitHub Actions、Terraform、SQL、智能合約一旦離開主語言也不會變黑洞。

底層概念模型來自 [GitNexus](https://github.com/abhigyanpatwari/GitNexus)（作者 [Abhigyan Patwari](https://github.com/abhigyanpatwari)），`ecp` 用 Rust 重寫成另一種讀者導向版本。

🎙️ **[Agent 訪談](../../interviews/README.md)** — Gemini CLI 與 Codex 在自主流程裡實測 `ecp` 的紀錄。

---

## ── 收據 ──

跟上游 GitNexus 對打，在 [gitnexus](https://github.com/abhigyanpatwari/GitNexus) 自家 codebase（TypeScript）上用 `scripts/parity/benchmark_vs_gitnexus.py` 量測：

| 階段 | ecp (Rust) | gitnexus (Node) | 加速 |
|---|---|---|---|
| **Cold Index** | **~970 ms** | ~58 s | **60×** |
| **Symbol Context** | **~70 ms** | ~430 ms | **6×** |
| **Blast Radius** | **~70 ms** | ~460 ms | **6×** |
| **Cypher Query** | **~70 ms** | ~400 ms | **5×** |

`ecp` 的數字包含完整 process 啟動時間（無 daemon）。GitNexus（v1.6.5）的數字是已 warm-up + indexed 後跑 CLI。

<details>
<summary><b>Scalability — <code>.sample_repo</code> 單次完整跑</b>（2.1 GB 多語言、~40 個 OSS 專案、25+ 語言）</summary>

**索引效能**

| 階段 | 值 |
|---|---|
| 索引檔案數 | **22,645**（25 種偵測到的語言） |
| Cold 牆鐘 | **2.60 s**（parse + resolve + serialize） |
| Incremental 牆鐘 | **4.9 ms**（xxh3_64 hash walk、零 dirty file） |
| 硬體 | AMD Ryzen 9 9950X（16 邏輯核）、39.2 GiB RAM、Linux 6.6.87 |

**單次查詢延遲**（包含 process 啟動）

| 查詢 | 中位數 | 備註 |
|---|---|---|
| `summary`（registry 總覽） | **1.4 ms** | 最小讀取 — 僅 mmap registry |
| `routes`（全 repo HTTP route map） | **142.3 ms** | 列舉 declarative + imperative |
| `summary --detailed`（框架 + blind-spot） | **143.4 ms** | 完整 registry + per-framework 打分 |
| `impact <symbol> --direction down` | **145.0 ms** | BFS 走 Calls / Extends |
| `inspect <symbol>`（signature + callers + callees） | **145.6 ms** | 符號解析 + 1-hop traversal |
| `find <name> --mode bm25`（lexical 搜尋） | **154.5 ms** | Tantivy 查詢 + 5 桶分區 |
| `cypher 'MATCH (a:Class)-[:HasMethod]->(b:Method) ...'` | **161.5 ms** | 單 pattern、單 row |
| `cypher 'MATCH (a:Method)-[:Calls]->(b:Method) ...'` | **174.2 ms** | 更廣 pattern、更多 match |
| `impact --baseline HEAD~1`（變更集 blast radius） | **359.0 ms** | git diff + 並行 per-file parse + BFS |

復現：`python scripts/benchmark/benchmark_ecp.py`。

</details>

---

## ── vs. upstream gitnexus ──

概念模型一樣、受眾不同。`ecp` **不是** drop-in 替代品 —— 依「誰要讀這張 graph」決定用哪個。

| 維度 | EgentCodePlexus | GitNexus |
|---|---|---|
| 主要消費者 | 自主 AI code agent | 真人開發者 + IDE 整合 |
| Runtime | 無狀態一次性 CLI（零暖機） | 長駐 MCP server |
| 性能 | **< 2.5 s cold index / < 150 ms query** | ~60 s cold index / ~400 ms query |
| 無法解析的 edge | `BlindSpot` 紀錄（誠實未知） | 啟發式猜測 |
| 預設輸出 | TOON / compact JSON（token 便宜） | Wiki / UI rendering |
| 語言 | 31（14 deep + 17 structural） | 14（deep, 9-dimension） |
| 儲存 | Rust + `rkyv` 零拷貝 mmap | Node.js + LadybugDB |

完整 8 維度分析 + 決策矩陣 → [docs/vs-gitnexus.md](../vs-gitnexus.md)。

---

## ── 30 秒 demo ──

```bash
$ ecp impact validateUser --direction upstream --format toon
```

```text
target          validateUser
  kind          Method
  file          src/auth/validate.py:42
risk_level      HIGH
direct_callers  3
  routes/api/login.py:18    POST /api/login   → loginUser
  routes/api/oauth.py:24    POST /api/oauth   → oauthLogin
  jobs/sync.py:91           sync_users (cron)
transitive      12 symbols across 4 files
blind_spots     1
  jobs/sync.py:103          dynamic dispatch via getattr (unresolved)
```

整個 round-trip 一個 process、一次 mmap、~140 ms。讀類指令支援 `--format text|json|toon`；每個指令的預設都選 token 最便宜的編碼。

---

## ── 安裝 ──

每次 GitHub Release 都發 prebuilt binary。Installer 腳本只有在找不到對應 release asset 時才會 fallback 到 cargo source build。

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# 直接走 cargo（跟 installer 同一個 source build）
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

<details>
<summary>CPU-tuned source build</summary>

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

</details>

---

## ── 快速上手 ──

```bash
# 1. Index 當前 repo（增量；首次查詢也會自動 index）
ecp admin index --repo .

# 2. 找符號 —— 預設 exact name
ecp find loginUser
ecp find login --mode bm25       # BM25 排序、top-K 分 source/tests/ref/doc/config 桶

# 3. 衝擊半徑 —— 動它會炸到誰？
ecp impact validateUser --direction upstream

# 4. 完整符號脈絡（簽名、body、callers、callees、1-hop impact）
ecp inspect validateUser

# 5. 全 repo HTTP route（declarative @Get + imperative app.get()）
ecp routes
ecp routes /api/users --method POST     # route → handler → caller chain
```

讀取端命令接受 `--format text|json|toon`。預設為該命令最省 Token 的格式（多數為 `toon`；`find` 預設為 `text`；`cypher` 預設為 `json`）。

---

## ── cli surface ──

兩層 —— **agent commands** 在頂層（query / refactor / verify），**admin commands** 在 `ecp admin` 下（registry / hooks / 破壞性操作）。完整 flag 矩陣請跑 `ecp --help` 與 `ecp admin --help`。

| 指令 | 用途 |
|---|---|
| `inspect <name>` | 單一符號 → metadata、decorator、signature、callers、callees、1-hop impact、enum variants |
| `find <pattern>` | 符號定位 —— exact（預設）· `--mode fuzzy` 子字串 · `--mode bm25` 詞彙排序；bm25 把輸出分到 source / tests / reference / document / config 5 個桶 |
| `find-schema-bindings <field>` | Schema field mirror 偵測（跨 service / 跨 model 的欄位對齊 + blind-spot 候選） |
| `find-transaction-patterns [--class <Name>]` | Saga compensate/undo/rollback 名稱對偵測（≥0.75 POSSIBLY_RELATED、<0.75 BLIND_SPOT） |
| `impact <name> --direction <up\|down>` | 衝擊半徑遍歷 + confidence 過濾。`--baseline <ref>` 做變更集 impact；`--literal <V>` 找 PathLiteral split-brain。 |
| `rename --symbol <old> --new-name <new>` | 14 語言 AST 感知多檔重命名。永遠先 `--dry-run`。 |
| `cypher '<query>'` | openCypher 後門；`m.content` 拿原始碼 body。 |
| `summary` | Registry 總覽、框架覆蓋、blind-spot 名單、graph 新鮮度。（原 `coverage`；舊名留作別名一個版本） |
| `routes [<path>]` | 列出 HTTP route（declarative + imperative）；給 `<path>` 就秀 handler + callers。 |
| `contracts` | 跨 repo API contract 庫存（routes / queue / RPC）。 |
| `diff` | Resolver delta —— edge 級別 binding tier-degradation + route / contract 變動。 |
| `tool-map` | 對外部 HTTP / DB / Redis / queue client 的呼叫（per-file import-binding 分析）。 |
| `shape-check` | HTTP consumer 存取模式 vs. Route response shape 的漂移。 |
| `peers` | 多 session 對端協作：`status` / `diff` / `say` / `inbox` / `log` / `thread` / `watch` / `gc`。 |
| `review` | LLM-workflow 稽核聚合器 —— impact + summary + tool-map + shape-check + diff，過濾到高信心訊號。 |

<details>
<summary><b>Admin namespace</b> —— <code>ecp admin &lt;cmd&gt;</code>（registry / hooks / 破壞性）</summary>

| 指令 | 用途 |
|---|---|
| `index --repo <path>` | 建 / 刷新 graph；xxh3_64 內容快取做增量。`--force` 全量重建。 |
| `drop / prune / rename-branch` | Index 生命週期：刪除、清理過時 branch dir、改名 on-disk branch。 |
| `install-hook` | 裝 git reference-transaction hook（自動追蹤 branch 切換）。 |
| `config` | `.ecp/config.toml` 互動式 TOML 精靈。 |
| `mcp serve` / `mcp tools` | MCP server（stdio）給 LLM host；`tools` 列出曝露的 tool 表面。 |
| `claude install / codex install / gemini install` | 可腳本化的 host 整合（skills、hooks、MCP entry）。 |
| `verify-resolver` | 解析器 dump 對 language oracle diff（ecp-dev QA 用）。 |

</details>

所有指令預設從 CWD 解析 `.ecp/graph.bin`，可用 `--graph <path>` 改寫。Agent 端的指令設計上 non-interactive —— 每個 flag 走 `--help`、每個輸出可解析。`ecp admin` 不帶 subcommand 開互動 admin TUI。

---

## ── MCP server ──

`ecp` 內建 MCP server，把核心指令以 MCP tool 形式曝露。會說 MCP 的 host（Claude Code、Cursor、Windsurf、Cline、Codex CLI、Gemini CLI）都能註冊 `ecp` 然後自主呼叫。

```bash
ecp admin mcp tools          # 預覽要曝露的 tools
ecp admin mcp serve          # 跑 server（預設 spawn mode）
```

Claude Code 手動 host 設定範例（`~/.config/claude-code/mcp-servers.json`）：

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

真人漸進路徑：

```text
ecp admin → Agent Integrations → MCP → <host> → install
```

AI agent 腳本路徑：

```bash
ecp admin claude install mcp-server
ecp admin gemini install skills
```

<details>
<summary><b>Codex CLI 原生整合</b>（跟 MCP 不一樣 —— 對 openai/codex fork 出 patch）</summary>

Codex 原生路徑不會改你正在跑的 Codex 安裝；它寫出一份 patch，你拿去套用在 `openai/codex` fork。

漸進路徑：

```text
ecp admin → Agent Integrations → Codex CLI → install → native-tools
```

內建 skills（同一條路徑）：

```text
ecp admin → Agent Integrations → Codex CLI → install → skills → all | ecp | simplify
```

Agent 腳本路徑：

```bash
ecp admin codex install native-tools
ecp admin codex install skills all
ecp admin codex install skills ecp
ecp admin codex install skills simplify
```

內建 skills 教 agent 怎麼選工作流，這是 command help 推不出來的：

| Skill | 何時用 |
|---|---|
| `ecp` | Agent 要判斷 graph 感知的 symbol / impact / route / contract / rename 工作流是否優於 grep / 讀檔。 |
| `simplify` | Agent 要 review 變更，應該從 `ecp impact`、blind-spot、egress、shape drift、resolver delta 出發，再讀原始 diff。 |

`native-tools` 元件會寫：

```text
~/.config/ecp/host-integration/codex-cli.patch
```

在你的 Codex CLI fork 套用：

```bash
cd /path/to/openai-codex-fork
git apply ~/.config/ecp/host-integration/codex-cli.patch
```

驗證已有 native 標記的 fork —— 設 `ECP_CODEX_CLI_CHECKOUT` 後查狀態：

```bash
ECP_CODEX_CLI_CHECKOUT=/path/to/openai-codex-fork ecp admin codex status
ecp admin codex uninstall native-tools
ecp admin codex uninstall skills all
```

</details>

---

## ── 架構 ──

```
crates/
├── ecp-core        零拷貝 graph（rkyv + mmap）、增量快取、graph 查詢
├── ecp-analyzer    Tree-sitter parsers、HTTP route 偵測、framework 信心評分
├── ecp-mcp         MCP server（stdio）—— 把核心指令當 tool 曝露
└── ecp-cli         `ecp` binary、Tantivy BM25 引擎、token 優化的輸出
```

Parse → resolve → serialize 過 MPSC channel 進單一 builder thread，組裝 graph 後寫出零拷貝 `.ecp/graph.bin`。讀路徑（`inspect`、`cypher`、`impact` …）直接 mmap 這個檔。xxh3_64 內容快取讓 22k 檔的 repo 增量重建維持亞秒級。

---

## ── 語言覆蓋 ──

31 個語言做結構級解析（functions / classes / methods / imports / calls）。其中 14 個 —— 原 GitNexus 那組 —— 拿到全深度覆蓋，涵蓋 imports、named bindings、exports、heritage、types、constructors、config、frameworks、entry points、calls、rename。其餘 17 個是 structural-only（Bash、Crystal、Cairo、Dockerfile、Docker Compose、GitHub Actions、HCL、Lua、Markdown、Move、Nim、Solidity、SQL、Verilog、Vyper、YAML、Zig）。

📊 [完整語言能力矩陣](../language-matrix.md) —— 各語言狀態與理由。

---

## ── 調校 ──

| 環境變數 | 預設 | 效果 |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216`（16 MiB） | Ingest 時略過超過此值的原始檔。把 worst-case worker RAM 鎖在 `num_threads × MAX`。 |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | 找 `*.csproj` 的目錄遞迴深度。.NET 深層 monorepo 可調高。 |

---

## ── 授權 ──

採用 [PolyForm Noncommercial 1.0.0](../../LICENSE.md)。個人使用、研究、業餘專案、非商業組織明確允許。**本授權不授予商業使用權** —— 商業授權請聯絡上游 GitNexus 作者 [Abhigyan Patwari](https://github.com/abhigyanpatwari)。必要的歸屬聲明見 [NOTICES.md](../../LICENSES/NOTICES.md)。

<details>
<summary><b>站在這些巨人肩上</b>（致謝）</summary>

- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — 原始設計、CLI 介面、概念模型
- [tree-sitter](https://tree-sitter.github.io/) — 增量 AST 解析
- [rkyv](https://rkyv.org/) — 零拷貝序列化框架
- [Tantivy](https://github.com/quickwit-oss/tantivy) — Rust BM25 全文搜尋引擎
- [Rayon](https://github.com/rayon-rs/rayon) — 多核心並行 AST 解析的資料並行庫
- [xxhash (xxh3_64)](https://xxhash.com/) — 內容雜湊驅動的增量索引
- [DashMap](https://github.com/xacrimon/dashmap) — 並行雜湊表（graph 組裝用）
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — 零拷貝 mmap，亞毫秒級 graph 讀取
- [msgspec](https://github.com/jcrist/msgspec) — IPC 用高效能 JSON 序列化

AI agent 安裝引導（URL bootstrap、Claude Code skill、plugin install）位於 `docs/skills/ecp-onboard/`。並行不變式與如何重新驗證：`./scripts/audit/audit-concurrency.sh`。

</details>

---

## ── 發佈狀態 ──

目前已驗證的安裝路徑是 `cargo install --git ...`，從原始碼建置 `ecp`。Release installer 已包含 checksum 與 provenance verification 流程，但必須等 tag 與 release assets 發佈後，binary 下載路徑才能做端到端驗證。Agent 安裝引導文件在 [docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md) —— 引導使用者完成安裝、首次索引、可選 group、MCP wiring、後續建議。輔助式設定流程仍在完善中。

---

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)
