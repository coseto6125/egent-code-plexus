# EgentCodePlexus

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)

[![Linux](https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black)](https://github.com/coseto6125/egent-code-plexus/releases)
[![macOS](https://img.shields.io/badge/macOS-000000?style=for-the-badge&logo=apple&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/releases)
[![Windows](https://img.shields.io/badge/Windows-0078D6?style=for-the-badge&logo=windows&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/releases)
[![Claude Code](https://img.shields.io/badge/Claude_Code-D97757?style=for-the-badge&logo=anthropic&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/skill_sample/claude/SKILL.md)
[![Codex CLI](https://img.shields.io/badge/Codex_CLI-412991?style=for-the-badge&logo=openai&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/skill_sample/codex/ecp/SKILL.md)
[![Cursor](https://img.shields.io/badge/Cursor-000000?style=for-the-badge&logo=cursor&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/docs/skills/ecp-onboard/guides/04-mcp.md)

`cold index 2.60 s · query p50 142 ms · 31 languages · BlindSpot edges (no hallucinated dispatch) · 60× upstream gitnexus`

[English](../../README.md) · [繁體中文](./README_zh-TW.md) · **简体中文** · [Español](./README_es.md) · [Русский](./README_ru.md) · [हिन्दी](./README_hi.md) · [日本語](./README_ja.md) · [한국어](./README_ko.md) · [Português (BR)](./README_pt-BR.md)

---

## 为什么

Code agent 一次任务里会做 20–50 次代码查找。Grep 给你字符串；自主代理需要的是符号、调用者、边（edge），与「graph 推不出来时诚实说不知道」的信号。

`ecp` 是这层结构知识，特性是：

- **无状态。** 每次调用 `mmap` 一个 `rkyv` 零拷贝 graph，跑完直接 exit。没有 daemon 要保温、没有「server 死了请重启」这种失败模式。
- **诚实。** 当调用点静态解析不出来（动态派发、未解析 import、reflection），`ecp` 发出 `BlindSpot` 纪录。代理对着幻觉出来的依赖下手，比代理收到一个「我不知道」然后绕道的成本高得多。
- **Token 便宜。** 缺省输出 TOON（紧凑 key:value）。每个 flag 都从 `--help` 出来、每个指令都 non-interactive 且 stdout 可解析。没有耗 context window 的 UI 杂讯。
- **多语言。** 31 个语言做结构级解析 —— service code、Dockerfile、GitHub Actions、Terraform、SQL、智能合约一旦离开主语言也不会变黑洞。

底层概念模型来自 [GitNexus](https://github.com/abhigyanpatwari/GitNexus)（作者 [Abhigyan Patwari](https://github.com/abhigyanpatwari)），`ecp` 用 Rust 重写成另一种读者导向版本。

🎙️ **[Agent 访谈](../../interviews/README.md)** — Gemini CLI 与 Codex 在自主流程里实测 `ecp` 的纪录。

## 收据

跟上游 GitNexus 对打，在 [gitnexus](https://github.com/abhigyanpatwari/GitNexus) 自家 codebase（TypeScript）上用 `scripts/parity/benchmark_vs_gitnexus.py` 量测：

| 阶段 | ecp (Rust) | gitnexus (Node) | 加速 |
|---|---|---|---|
| **Cold Index** | **~970 ms** | ~58 s | **60×** |
| **Symbol Context** | **~70 ms** | ~430 ms | **6×** |
| **Blast Radius** | **~70 ms** | ~460 ms | **6×** |
| **Cypher Query** | **~70 ms** | ~400 ms | **5×** |

`ecp` 的数字包含完整 process 启动时间（无 daemon）。GitNexus（v1.6.5）的数字是已 warm-up + indexed 后跑 CLI。

<details>
<summary><b>Scalability — <code>.sample_repo</code> 单次完整跑</b>（2.1 GB 多语言、~40 个 OSS 项目、25+ 语言）</summary>

**索引性能**

| 阶段 | 值 |
|---|---|
| 索引文件数 | **22,645**（25 种侦测到的语言） |
| Cold 墙钟 | **2.60 s**（parse + resolve + serialize） |
| Incremental 墙钟 | **4.9 ms**（xxh3_64 hash walk、零 dirty file） |
| 硬件 | AMD Ryzen 9 9950X（16 逻辑核）、39.2 GiB RAM、Linux 6.6.87 |

**单次查找延迟**（包含 process 启动）

| 查找 | 中位数 | 备注 |
|---|---|---|
| `coverage`（registry 总览） | **1.4 ms** | 最小读 — 只 mmap registry |
| `routes`（HTTP route map 全 repo） | **142.3 ms** | 枚举 declarative + imperative |
| `coverage --detailed`（框架 + blind-spot） | **143.4 ms** | 完整 registry + per-framework 打分 |
| `impact <symbol> --direction down` | **145.0 ms** | BFS 走 Calls / Extends |
| `inspect <symbol>`（signature + callers + callees） | **145.6 ms** | 符号解析 + 1-hop traversal |
| `find <name> --mode bm25`（lexical 搜索） | **154.5 ms** | Tantivy 查找 + 5 桶分区 |
| `cypher 'MATCH (a:Class)-[:HasMethod]->(b:Method) ...'` | **161.5 ms** | 单 pattern、单 row |
| `cypher 'MATCH (a:Method)-[:Calls]->(b:Method) ...'` | **174.2 ms** | 更广 pattern、更多 match |
| `impact --baseline HEAD~1`（变更集 blast radius） | **359.0 ms** | git diff + 并行 per-file parse + BFS |

复现：`python scripts/benchmark/benchmark_ecp.py`。

</details>

## vs. upstream gitnexus

概念模型一样、受众不同。`ecp` **不是** drop-in 替代品 —— 依「谁要读这张 graph」决定用哪个。

| 维度 | EgentCodePlexus | GitNexus |
|---|---|---|
| 主要消费者 | 自主 AI code agent | 真人开发者 + IDE 集成 |
| Runtime | 无状态一次性 CLI（零暖机） | 长驻 MCP server |
| 性能 | **< 2.5 s cold index / < 150 ms query** | ~60 s cold index / ~400 ms query |
| 无法解析的 edge | `BlindSpot` 纪录（诚实未知） | 启发式猜测 |
| 缺省输出 | TOON / compact JSON（token 便宜） | Wiki / UI rendering |
| 语言 | 31（14 deep + 17 structural） | 14（deep, 9-dimension） |
| 保存 | Rust + `rkyv` 零拷贝 mmap | Node.js + LadybugDB |

完整 8 维度分析 + 决策矩阵 → [docs/vs-gitnexus.md](../vs-gitnexus.md)。

## 30 秒 demo

```bash
$ ecp impact parse_with_budget --direction upstream --format toon
```

```text
target          parse_with_budget
  kind          Function
  file          crates/ecp-analyzer/src/parse_budget.rs:28
risk_level      HIGH
direct_callers  22 across 22 files
  crates/ecp-analyzer/src/python/parser.rs:48      Method parse_file
  crates/ecp-analyzer/src/rust/parser.rs:142       Method parse_file
  crates/ecp-analyzer/src/typescript/parser.rs:73  Method parse_file
  crates/ecp-analyzer/src/go/parser.rs:69          Method parse_file
  ... (18 more language parsers)
transitive      231 symbols across language detection + pipeline
blind_spots     0
```

整个 round-trip 一个 process、一次 mmap、~140 ms。读类指令支持 `--format text|json|toon`；每个指令的缺省都选 token 最便宜的编码。

## 安装

每次 GitHub Release 都发 prebuilt binary。Installer 脚本只有在找不到对应 release asset 时才会 fallback 到 cargo source build。

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# 直接走 cargo（跟 installer 同一个 source build）
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

<details>
<summary>CPU-tuned source build</summary>

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

</details>

## 快速上手

```bash
# 1. Index 当前 repo（增量；首次查找也会自动 index）
ecp admin index --repo .

# 2. 找符号 —— 缺省 exact name
ecp find loginUser
ecp find login --mode bm25       # BM25 排序、top-K 分 source/tests/ref/doc/config 桶

# 3. 冲击半径 —— 动它会炸到谁？
ecp impact validateUser --direction upstream

# 4. 完整符号脉络（签名、body、callers、callees、1-hop impact）
ecp inspect validateUser

# 5. 全 repo HTTP route（declarative @Get + imperative app.get()）
ecp routes
ecp routes /api/users --method POST     # route → handler → caller chain
```

## cli surface

两层 —— **agent commands** 在顶层（query / refactor / verify），**admin commands** 在 `ecp admin` 下（registry / hooks / 破坏性操作）。完整 flag 矩阵请跑 `ecp --help` 与 `ecp admin --help`。

| 指令 | 用途 |
|---|---|
| `inspect <name>` | 单一符号 → metadata、decorator、signature、callers、callees、1-hop impact |
| `find <pattern>` | 符号定位 —— exact（缺省）· `--mode fuzzy` 子字符串 · `--mode bm25` 词汇排序；bm25 把输出分到 source / tests / reference / document / config 5 个桶 |
| `impact <name> --direction <up\|down>` | 冲击半径遍历 + confidence 过滤。`--baseline <ref>` 做变更集 impact。 |
| `rename --symbol <old> --new-name <new>` | 14 语言 AST 感知多档重命名。永远先 `--dry-run`。 |
| `cypher '<query>'` | openCypher 后门；`m.content` 拿原代码 body。 |
| `coverage` | Registry 总览、框架覆盖、blind-spot 名单、graph 新鲜度。 |
| `routes [<path>]` | 列出 HTTP route（declarative + imperative）；给 `<path>` 就秀 handler + callers。 |
| `contracts` | 跨 repo API contract 库存（routes / queue / RPC）。 |
| `diff` | Resolver delta —— edge 级别 binding tier-degradation + route / contract 变动。 |
| `tool-map` | 对外部 HTTP / DB / Redis / queue client 的调用（per-file import-binding 分析）。 |
| `shape-check` | HTTP consumer 访问模式 vs. Route response shape 的漂移。 |
| `peers` | 多 session 对端协作（status / diff / log / gc）。 |
| `review` | LLM-workflow 稽核聚合器 —— impact + coverage + tool-map + shape-check + diff，过滤到高信心信号。 |

<details>
<summary><b>Admin namespace</b> —— <code>ecp admin &lt;cmd&gt;</code>（registry / hooks / 破坏性）</summary>

| 指令 | 用途 |
|---|---|
| `index --repo <path>` | 建 / 刷新 graph；xxh3_64 内容缓存做增量。`--force` 全量重建。 |
| `drop / prune / rename-branch` | Index 生命周期：删除、清理过时 branch dir、改名 on-disk branch。 |
| `install-hook` | 装 git reference-transaction hook（自动追踪 branch 切换）。 |
| `config` | `.ecp/config.toml` 交互式 TOML 精灵。 |
| `mcp serve` / `mcp tools` | MCP server（stdio）给 LLM host；`tools` 列出曝露的 tool 表面。 |
| `claude install / codex install / gemini install` | 可脚本化的 host 集成（skills、hooks、MCP entry）。 |
| `verify-resolver` | 解析器 dump 对 language oracle diff（ecp-dev QA 用）。 |

</details>

所有指令缺省从 CWD 解析 `.ecp/graph.bin`，可用 `--graph <path>` 改写。Agent 端的指令设计上 non-interactive —— 每个 flag 走 `--help`、每个输出可解析。`ecp admin` 不带 subcommand 开交互 admin TUI。

## MCP server

`ecp` 内置 MCP server，把内核指令以 MCP tool 形式曝露。会说 MCP 的 host（Claude Code、Cursor、Windsurf、Cline、Codex CLI、Gemini CLI）都能注册 `ecp` 然后自主调用。

```bash
ecp admin mcp tools          # 预览要曝露的 tools
ecp admin mcp serve          # 跑 server（缺省 spawn mode）
```

Claude Code 手动 host 设置范例（`~/.config/claude-code/mcp-servers.json`）：

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

真人渐进路径：

```text
ecp admin → Agent Integrations → MCP → <host> → install
```

AI agent 脚本路径：

```bash
ecp admin claude install mcp-server
ecp admin gemini install skills
```

<details>
<summary><b>Codex CLI 原生集成</b>（跟 MCP 不一样 —— 对 openai/codex fork 出 patch）</summary>

Codex 原生路径不会改你正在跑的 Codex 安装；它写出一份 patch，你拿去套用在 `openai/codex` fork。

渐进路径：

```text
ecp admin → Agent Integrations → Codex CLI → install → native-tools
```

内置 skills（同一条路径）：

```text
ecp admin → Agent Integrations → Codex CLI → install → skills → all | ecp | simplify
```

Agent 脚本路径：

```bash
ecp admin codex install native-tools
ecp admin codex install skills all
ecp admin codex install skills ecp
ecp admin codex install skills simplify
```

内置 skills 教 agent 怎么选工作流，这是 command help 推不出来的：

| Skill | 何时用 |
|---|---|
| `ecp` | Agent 要判断 graph 感知的 symbol / impact / route / contract / rename 工作流是否优于 grep / 读档。 |
| `simplify` | Agent 要 review 变更，应该从 `ecp impact`、blind-spot、egress、shape drift、resolver delta 出发，再读原始 diff。 |

`native-tools` 组件会写：

```text
~/.config/ecp/host-integration/codex-cli.patch
```

在你的 Codex CLI fork 套用：

```bash
cd /path/to/openai-codex-fork
git apply ~/.config/ecp/host-integration/codex-cli.patch
```

验证已有 native 标记的 fork —— 设 `ECP_CODEX_CLI_CHECKOUT` 后查状态：

```bash
ECP_CODEX_CLI_CHECKOUT=/path/to/openai-codex-fork ecp admin codex status
ecp admin codex uninstall native-tools
ecp admin codex uninstall skills all
```

</details>

## 架构

```
crates/
├── ecp-core        零拷贝 graph（rkyv + mmap）、增量缓存、graph 查找
├── ecp-analyzer    Tree-sitter parsers、HTTP route 侦测、framework 信心评分
├── ecp-mcp         MCP server（stdio）—— 把内核指令当 tool 曝露
└── ecp-cli         `ecp` binary、Tantivy BM25 引擎、token 优化的输出
```

Parse → resolve → serialize 过 MPSC channel 进单一 builder thread，组装 graph 后写出零拷贝 `.ecp/graph.bin`。读路径（`inspect`、`cypher`、`impact` …）直接 mmap 这个档。xxh3_64 内容缓存让 22k 档的 repo 增量重建维持亚秒级。

## 语言覆盖

31 个语言做结构级解析（functions / classes / methods / imports / calls）。其中 14 个 —— 原 GitNexus 那组 —— 拿到全深度覆盖，涵盖 imports、named bindings、exports、heritage、types、constructors、config、frameworks、entry points、calls、rename。其余 17 个是 structural-only（Bash、Crystal、Cairo、Dockerfile、Docker Compose、GitHub Actions、HCL、Lua、Markdown、Move、Nim、Solidity、SQL、Verilog、Vyper、YAML、Zig）。

📊 [完整语言能力矩阵](../language-matrix.md) —— 各语言状态与理由。

## 调校

| 环境变量 | 缺省 | 效果 |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216`（16 MiB） | Ingest 时略过超过此值的源文件。把 worst-case worker RAM 锁在 `num_threads × MAX`。 |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | 找 `*.csproj` 的目录递归深度。.NET 深层 monorepo 可调高。 |

## 授权

采用 [PolyForm Noncommercial 1.0.0](../../LICENSE.md)。个人使用、研究、业余项目、非商业组织明确允许。**本授权不授予商业使用权** —— 商业授权请联系上游 GitNexus 作者 [Abhigyan Patwari](https://github.com/abhigyanpatwari)。必要的归属声明见 [NOTICES.md](../../LICENSES/NOTICES.md)。

<details>
<summary><b>站在这些巨人肩上</b>（致谢）</summary>

- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — 原始设计、CLI 接口、概念模型
- [tree-sitter](https://tree-sitter.github.io/) — 增量 AST 解析
- [rkyv](https://rkyv.org/) — 零拷贝串行化框架
- [Tantivy](https://github.com/quickwit-oss/tantivy) — Rust BM25 全文搜索引擎
- [Rayon](https://github.com/rayon-rs/rayon) — 多内核并行 AST 解析的数据并行库
- [xxhash (xxh3_64)](https://xxhash.com/) — 内容哈希驱动的增量索引
- [DashMap](https://github.com/xacrimon/dashmap) — 并行哈希表（graph 组装用）
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — 零拷贝 mmap，亚毫秒级 graph 读取
- [msgspec](https://github.com/jcrist/msgspec) — IPC 用高性能 JSON 串行化

AI agent 安装引导（URL bootstrap、Claude Code skill、plugin install）位于 `docs/skills/ecp-onboard/`。并行不变式与如何重新验证：`./scripts/audit/audit-concurrency.sh`。

</details>

## 发布状态

目前已验证的安装路径是 `cargo install --git ...`，从原代码建置 `ecp`。Release installer 已包含 checksum 与 provenance verification 流程，但必须等 tag 与 release assets 发布后，binary 下载路径才能做端到端验证。Agent 安装引导文档在 [docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md) —— 引导用户完成安装、首次索引、可选 group、MCP wiring、后续建议。辅助式设置流程仍在完善中。

---

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)
