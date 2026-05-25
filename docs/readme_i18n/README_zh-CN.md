<div align="center">

# `ecp` · EgentCodePlexus

### 为 AI 代理而生、而非为人类打造的结构化代码图谱。

*2.2 万个文件 2.6 秒完成索引 · 任何查询 &lt;175 ms 内响应 · 诚实的未知，绝不捏造边。*

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)
![Cold index 2.6s](https://img.shields.io/badge/cold_index-2.6s%20%2F%2022k%20files-brightgreen)
![Query latency](https://img.shields.io/badge/query-%3C175ms%20cold-blue)
![Languages](https://img.shields.io/badge/languages-31%20parsed-orange)
![License](https://img.shields.io/badge/license-PolyForm%20NC-lightgrey)
![Built with Rust](https://img.shields.io/badge/built_with-Rust-orange?logo=rust)
![Status early release](https://img.shields.io/badge/status-early%20release-yellow)

[English](../../README.md) · [繁體中文](./README_zh-TW.md) · **简体中文** · [日本語](./README_ja.md) · [한국어](./README_ko.md) · [Español](./README_es.md) · [Português](./README_pt-BR.md) · [Русский](./README_ru.md) · [हिन्दी](./README_hi.md)

</div>

---

自主代码代理在**每个任务中发起 20–50 次结构化查询**。这些查询全都打在为人类打造的工具上：IDE 侧边栏、需要预热的守护进程、为人眼阅读而排版的输出。这种错配具体表现为三种故障模式：

1. **Token 浪费** — `grep` 倾倒了 400 行，而代理只需要其中 10 个符号
2. **破坏性重构** — 解析器猜错，漏掉一个调用者就此溜过
3. **幻觉依赖** — 当静态分析无法触及某条边时，工具干脆捏造一条

`ecp` 就是为了消除这三者而生。

| 故障模式 | `ecp` 的解法 |
|---|---|
| 原始搜索输出炸掉上下文窗口 | **TOON / 精简 JSON** — 只给符号、行号与边，毫无填充 |
| 漏掉调用者，下游无声崩坏 | **`impact`** — 在真实的调用与继承边上计算精确的影响范围 |
| 在代理推理中混入捏造的依赖 | **`BlindSpot` 记录** — 带类型、可绕道的诚实未知 |
| 一离开主语言图谱就变黑洞 | **31 种语言** — 服务代码、IaC、SQL、智能合约一次遍历全覆盖 |

---

## 🎯 设计原则

每个设计决策都源于同一个问题：*接收方代理究竟需要什么？*

**输出是数据结构。** TOON 与精简 JSON 只携带代理做下一步决策所需的内容。没有散文摘要、没有视觉装饰、没有吃掉上下文预算的章节标题。各命令的格式默认值，对多数 LLM prompt 而言已经是正确选择。

**无状态、零预热。** 每次调用都 `mmap` 一个零拷贝的 `rkyv` 图谱文件后退出。**每次查询 ~140–170 ms，已含启动时间。** 没有要维持存活的守护进程、没有预热阶段、没有「服务器崩溃请重启」的恢复路径。代理可以在不付出进程启动成本的前提下，每个任务发起 50 次查询。

**宁可 BlindSpot，也不要幻觉。** 当 `ecp` 无法静态解析某个调用点时——动态分派、反射、未解析的导入——它会发出一条 `BlindSpot` 记录：一个带名称、带类型、明确标示的图谱缺口。代理能绕过已知的未知，却无法从一个自信的捏造中恢复。

**默认多语言。** 31 种语言的结构深度解析。服务代码、Dockerfile、GitHub Actions、Terraform、SQL、Move、Solidity——一次遍历即覆盖所有层。不必切换语言，也就不会出现图谱盲区。

🎙️ **[Agent 访谈记录](../../interviews/README.md)** — Gemini CLI 与 Codex 描述它们在实际自主任务流中如何使用 `ecp`。

致敬 [GitNexus](https://github.com/abhigyanpatwari/GitNexus)（原作 [Abhigyan Patwari](https://github.com/abhigyanpatwari)）——同样的结构化图谱概念，用 Rust 重写，面向不同受众。授权 [PolyForm Noncommercial 1.0.0](../../LICENSE.md)；必要的归属清单见 [NOTICES.md](../../LICENSES/NOTICES.md)。

---

## ⚡ 实测数据

三方实测对决：[`codegraph`](https://github.com/colbymchenry/codegraph)（Node + SQLite）与上游 [`gitnexus`](https://github.com/abhigyanpatwari/GitNexus)（Node）——相同 checkout、相同机器。`ecp` 是无状态一次性 CLI：以下所有延迟**皆含完整进程启动**，无守护进程、无预热。

*版本：`ecp` 0.4.2 · `codegraph` 0.9.4 · `gitnexus` 1.6.5。所有工具在可配置时均以 1 MiB 最大文件大小为上限（`gitnexus` 硬编码 512 KB）。`ecp` 取 5–7 次执行中位数。硬件：AMD Ryzen 9 9950X（16 逻辑核心）、Linux。*

### `microsoft/vscode` — 14,874 个文件、密集单语言 TypeScript

| 指标 | **`ecp`** | `codegraph` | `gitnexus` |
|---|---|---|---|
| **冷启动索引** | **4.6 s** | 166.9 s | **DNF** — 27 分钟后强制终止 |
| 内存峰值 RSS | **~1.0 GiB** | 1.7 GiB | 4.6 GiB（仍在攀升） |
| 符号查找 / 查询 | **34.6 ms** | 169.5 ms | — |
| 调用者 / 影响范围 | **27.2 ms** | 172.4 ms | — |
| 检视 / 上下文 | **35.0 ms** | 415.9 ms | — |
| 影响基准（git-diff） | **725.9 ms** | N/A — 无此模式 | — |
| 图节点数 | **507,257** | 315,498 | — |
| 图边数 | 916,380 | **986,709** | — |
| 磁盘索引大小 | **87 MiB** | 671 MiB | — |
| 已索引文件数 | **14,874** | 10,814 | — |

*`gitnexus` 未完成——在内存内图解析阶段卡住 27 分钟后强制终止（RSS 4.6 GiB，无输出写入）。*

### `abhigyanpatwari/GitNexus` — 3,232 个文件、多语言（三者均能完成的语料）

| 指标 | **`ecp`** | `codegraph` | `gitnexus` |
|---|---|---|---|
| **冷启动索引** | **0.74 s** | 11.2 s | 77.6 s |
| 内存峰值 RSS | **264 MiB** | 501 MiB | 2.5 GiB |
| 查找 / 查询 | **9.4 ms** | 103.5 ms | — |
| 调用者 / 影响范围 | **9.2 ms** | 104.2 ms | 297.6 ms |
| 检视 / 上下文 | **9.4 ms** | — | 295.5 ms |
| 图节点数 | **49,122** | 19,604 | 30,223 |
| 图边数 | **48,271** | 39,155 | 47,218 |
| 磁盘索引大小 | **7.7 MiB** | 37 MiB | 306 MiB |
| 已索引文件数 | **3,232** | 2,968 | 3,232 |

**冷启动索引：比 `codegraph` 快 15–37×；`gitnexus` 在真实大型 repo 上无法完成。内存最低、磁盘索引最小、图最密——在各种规模下皆如此。**

### 规模：`.sample_repo` — 22,645 个文件、25 种语言、2.1 GB 多语言语料

**索引摄取：**

| 指标 | 数值 |
|---|---|
| 索引文件数 | **22,645** 个，横跨 25 种检测到的语言 |
| 冷启动摄取 | **2.60 s**（解析 + 解析绑定 + 序列化） |
| 增量摄取 | **4.9 ms**（xxh3_64 哈希遍历，零脏文件） |
| 硬件 | AMD Ryzen 9 9950X（16 逻辑核心）、39.2 GiB RAM、Linux 6.6.87 |

**每次查询延迟，已含进程启动：**

| 查询 | 中位数 | 涵盖内容 |
|---|---|---|
| `summary` | **1.4 ms** | registry mmap — 最小的读取 |
| `routes` | **142.3 ms** | 声明式 + 命令式路由枚举 |
| `summary --detailed` | **143.4 ms** | 完整 registry + 各框架置信度评分 |
| `impact --direction down` | **145.0 ms** | 在 Calls / Extends 边上做 BFS |
| `inspect` | **145.6 ms** | 符号解析 + 一跳遍历 |
| `find --mode bm25` | **154.5 ms** | Tantivy 查询 + 5 桶分区 |
| `cypher`（窄查询） | **161.5 ms** | 单一模式、单行结果 |
| `cypher`（宽查询） | **174.2 ms** | 较宽模式、更多匹配 |
| `impact --baseline HEAD~1` | **359.0 ms** | git diff + 每文件并行解析 + BFS |

完整复现：`python scripts/benchmark/benchmark_ecp.py`。

### 与 Rust 同级竞品的比较

`scripts/benchmark/benchmark_vs_competitors.py` 针对 [`codescope`](https://github.com/onur-gokyildiz-bhi/codescope)（SurrealDB 后端）与 `coraline`（SQLite 后端）横跨 6 个阶段测试：`cold-index`、`symbol-find`、`callers`、`file-context`、`route-map`、`cypher`。缺少的阶段标为 `N/A`（缺席本身就是信号）。结果会重新生成 `docs/benchmark-vs-competitors.md`。

```bash
python scripts/benchmark/benchmark_vs_competitors.py
python scripts/benchmark/benchmark_vs_competitors.py --corpus path/to/repo --iterations 5 --no-plot
```

---

## 🆚 对比上游 GitNexus

同样的结构化图谱概念，不同的受众。并非即插即用的替代品——依「谁读取输出、用它做什么」来选择。

| 维度 | EgentCodePlexus | GitNexus |
|---|---|---|
| 主要使用者 | 自主 AI 代码代理 | 人类开发者 + IDE 集成 |
| 运行模型 | 无状态单次 CLI（零预热） | 长驻 MCP 服务器 |
| 性能 | **< 2.5s 冷索引 / < 175ms 查询** | ~60s 冷索引 / ~400ms 查询 |
| 未解析的边 | `BlindSpot` 记录（诚实的未知） | 启发式猜测 |
| 默认输出 | TOON / 精简 JSON（省 Token） | Wiki / UI 渲染 |
| 语言 | 31（14 深度 + 17 结构） | 14（深度、9 维度） |
| 存储 | Rust + `rkyv` 零拷贝 mmap | Node.js + LadybugDB |

**完整拆解、设计哲学与决策矩阵 → [docs/vs-gitnexus.md](../vs-gitnexus.md)**

---

## 📦 安装

预编译二进制文件随每次 GitHub Release 发布。当找不到对应的发布资产时，安装脚本才会回退到 cargo 源码构建。

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# 直接走 cargo（不经安装脚本包装）
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

CPU 调优的源码构建：

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

---

## 🚀 快速上手

无守护进程需启动。无需配置。一个命令，从零到可查询的图谱。

```bash
# 索引（增量；若无索引，首次查询会自动建立）
ecp admin index --repo .

# 找符号 — 默认精确匹配
ecp find loginUser
ecp find login --mode bm25            # BM25 排序，分成 5 个输出桶

# 影响范围 — 改这个会弄坏谁？
ecp impact validateUser --direction upstream

# 完整符号上下文（签名、本体、调用者、被调用者、一跳影响）
ecp inspect validateUser

# HTTP 路由地图（声明式 @Get + 命令式 app.get()）
ecp routes
ecp routes /api/users --method POST   # 路由 → 处理器 → 调用链

# 文件使用情况：谁读 / 写这个路径？
ecp impact --literal session_meta.json
```

所有读取端命令都接受 `--format text|json|toon`。各命令的默认值为最省 Token 的表示（多为 `toon`；`find` 默认 `text`；`cypher`/`summary` 默认 `json`）。

---

## 🛠️ CLI 命令面

两层架构：顶层的**代理命令**（查询 / 重构 / 验证），以及 `ecp admin` 之下的**管理命令**（registry / hooks / 破坏性操作）。运行 `ecp --help` 与 `ecp admin --help` 查看完整标志矩阵。

**代理命令：**

| 命令 | 用途 |
|---|---|
| `inspect <name>` | 符号 → 元数据、装饰器、签名、调用者、被调用者、一跳影响、所含方法 / 属性 / enum 变体 |
| `find <pattern>` | 精确 · `--mode fuzzy` · `--mode bm25`（5 桶：source / tests / reference / document / config） |
| `find-schema-bindings <field>` | 跨类 / 服务的 MirrorsField 启发式边 + blind-spot 候选 |
| `find-transaction-patterns [--class <Name>]` | Saga compensate/undo/rollback 名称配对；≥0.75 → POSSIBLY_RELATED，<0.75 → BLIND_SPOT |
| `impact <name> --direction <up\|down>` | 带置信度过滤的影响范围 BFS；`--since <ref>` 计算变更集影响 |
| `rename --symbol <old> --new-name <new>` | 跨 14 种语言、AST 感知的多文件重命名。务必先 `--dry-run`。 |
| `cypher '<query>'` | openCypher 逃生口；`m.content` 返回源码本体 |
| `summary` | Registry 总览、框架覆盖、LLM 可行动的 blind-spot 目录、图谱新鲜度 |
| `routes [<path>]` | HTTP 路由枚举（声明式 + 命令式）；带 `<path>` 时显示处理器 + 调用链 |
| `contracts` | 跨 repo 的 API 契约清单（routes / queue / RPC） |
| `diff` | 解析器差异：绑定层级降级 + 路由 / 契约变更 |
| `tool-map` | 通过导入绑定分析找出外部 HTTP / DB / Redis / queue 的调用点 |
| `shape-check` | HTTP 消费者访问模式与 Route 响应结构之间的漂移 |
| `peers` | 多会话协作：`status / diff / say / inbox / log / thread / watch / gc` |
| `review` | 一次性审查：impact + summary + tool-map + shape-check + diff，只保留高置信度信号 |

**管理命令**（`ecp admin <cmd>`）：

| 命令 | 用途 |
|---|---|
| `index --repo <path>` | 建立 / 刷新图谱；经由 xxh3_64 内容缓存做增量。`--force` 完整重建。 |
| `drop / prune / rename-branch` | 索引生命周期：删除、清理过时分支目录、就地重命名分支 |
| `install-hook` | Git reference-transaction hook（自动追踪分支切换） |
| `config` | `.ecp/config.toml` 的交互式 TOML 向导 |
| `mcp serve` / `mcp tools` | MCP 服务器（stdio）；`tools` 列出对外暴露的工具面 |

除非提供 `--graph <path>`，所有命令都从 CWD 解析 `.ecp/graph.bin`。每个面向代理的命令都是非交互式的；每个输出流都可解析。

### 多会话伙伴同步

当多个 LLM 会话并行编辑同一个 repo 时，`ecp peers` 会揭露各会话的符号层级脏状态，并支持会话间直接传讯。通过 `ECP_SESSION_ID`、`CODEX_SESSION_ID`、`CODEX_THREAD_ID` 或 `CLAUDE_CODE_SESSION_ID` 注册。

```bash
# 启动 watcher（每会话一个；inbox 推送事件所必需）
ecp peers watch --start

# 现在还有谁在编辑？
ecp peers status                                  # text
ecp peers status --format json                    # {session_id, pid, watcher: alive|dead|not-started}

# 检视某个伙伴的脏符号
ecp peers diff <peer-session-id> [<symbol>]

# 发送消息
ecp peers say "rebasing on main, hold pushes 5min"    # 广播
ecp peers say --to <peer-session-id> "take auth.rs?"  # 定向

# 读取与管理
ecp peers inbox
ecp peers log --limit 20
ecp peers thread <msg-id>

# 清理
ecp peers watch --stop && ecp peers gc
```

`watcher` 字段区分 `alive` | `dead` | `not-started`——崩溃不会伪装成「功能未被使用」。

### 可证明的代码审查裁决

`ecp review --verdicts` 从 `ecp diff` 的各区段预先计算出图谱支撑的裁决。把 JSON 直接当作审查上下文传入——免去 LLM 从原始 diff 重新推导调用者关系。

```bash
ecp review --since main --verdicts --format json
```

| 严重度 | 规则 |
|---|---|
| `RISK` | 存在跨文件调用者、移除了公开符号，或 diff 区域内有 blindspot |
| `WARN` | 只有文件内调用者，或路由被修改 |
| `INFO` | 找不到调用者，或新增了公开表面 |

裁决种类：`SIGNATURE_OR_BODY_CHANGED` · `NEW_PUBLIC_SURFACE` · `REMOVED_PUBLIC_SURFACE` · `ROUTE_CONTRACT_CHANGED` · `BLINDSPOT_IN_DIFF_REGION`

每条裁决都会引用触发它的确切 diff 区段与图谱事实。完整规格：[docs/specs/2026-05-22-review-verdicts.md](../specs/2026-05-22-review-verdicts.md)。

---

## 🔌 代理集成

**有原生路径时优先采用**——它会接上自动重建索引的 hooks 与工作流 skill，教代理*何时*值得为一次图谱查询付出往返成本。**MCP 是通用回退**，适用于任何会说该协议的 host。

| 代理 | 路径 | 接上的能力 |
|---|---|---|
| Claude Code | 原生 | hooks + skills + 可选 MCP |
| Codex CLI | 原生 | skills（native-tools 尚待接线） |
| Gemini CLI | 原生 | 原生 skill **或** MCP |
| Cursor · Windsurf · Cline · Copilot · 任何 MCP host | MCP | MCP 服务器 |

引导式配置：`ecp admin → Agent Integrations → <host>`。给自动化用的可脚本化路径：`ecp admin <host> install <component>`。检视任何 host：`ecp admin <host> status`。

### Claude Code

```bash
ecp admin claude install hooks          # settings.json：自动重建索引 + 上下文增强
ecp admin claude install skills all     # ecp + simplify skill 包（或：ecp | simplify）
ecp admin claude install mcp-server     # 可选 — hooks + skills + CLI 已足够
```

Hooks 会在每次 Grep/Glob/Bash 时喂给图谱上下文，无需明确的工具调用。`ecp` skill 教会 symbol / impact / route / contract / rename 工作流。`simplify` 驱动图谱优先的代码审查。

### Gemini CLI

```bash
ecp admin gemini install native-skill   # 经 `gemini skills link` 链接
ecp admin gemini install mcp-server     # 经 `gemini mcp add` 注册
```

`native-skill` 与 `mcp-server` 互斥——安装其一会移除另一个。

### Codex CLI

```bash
ecp admin codex install skills all      # ecp + simplify；native-tools 待 Codex 接线
```

**工作流 skill：**

| Skill | 何时使用 |
|---|---|
| `ecp` | 代理需判断在符号、调用者、路由、契约上，图谱感知工作流是否胜过 grep / 读文件 |
| `simplify` | 从 ecp impact、blind spots、egress、shape drift、解析器差异出发的代码审查 |

### MCP 回退（Cursor、Windsurf、Cline、任何 MCP host）

| Host | 配置文件 |
|---|---|
| Cursor | `~/.cursor/mcp.json` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` |
| Cline (VS Code) | `cline_mcp_settings.json`（MCP 面板 → "Edit MCP Settings"） |
| 通用 MCP host | 视 host 而定 |

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

```bash
ecp admin mcp tools    # 连接前先验证暴露的工具面
ecp admin mcp serve    # 每次调用无状态单次执行（零预热成本）
```

---

## 🏗️ 架构

```
crates/
├── ecp-core        # 零拷贝图谱（rkyv + mmap）、增量缓存、图谱查询
├── ecp-analyzer    # Tree-sitter 解析器、HTTP 路由检测器、框架置信度评分
├── ecp-mcp         # MCP 服务器（stdio）— 将核心命令暴露为工具
└── ecp-cli         # `ecp` 二进制文件、Tantivy BM25 引擎、Token 优化输出
```

解析 → 解析绑定 → 序列化，全程通过一个 MPSC channel 汇入单一构建线程，由它组装图谱并写出零拷贝的 `.ecp/graph.bin`。读取路径（`inspect`、`cypher`、`impact`…）直接 mmap 这个文件——无反序列化步骤。xxh3_64 内容缓存让 2.2 万文件 repo 的增量重建维持在亚秒级。

---

## 🌐 语言覆盖

31 种语言的结构层级解析。**14 种完整深度**（TypeScript、JavaScript、Python、Java、Kotlin、C#、Go、Rust、PHP、Ruby、Swift、C、C++、Dart）——涵盖导入、具名绑定、导出、继承、类型、构造函数、配置、框架、入口点、调用与重命名。**17 种仅结构**：Bash、Crystal、Cairo、Dockerfile、Docker Compose、GitHub Actions、HCL、Lua、Markdown、Move、Nim、Solidity、SQL、Verilog、Vyper、YAML、Zig。

📊 **[完整语言能力矩阵](../language-matrix.md)** — 各语言状态与理由。

---

## ⚙️ 调优

| 环境变量 | 默认 | 效果 |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216`（16 MiB） | 摄取时跳过大于此大小的源码文件。将最坏情况下的 worker RAM 上限定在 `num_threads × MAX`。 |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | `*.csproj` 探索的目录递归深度。深层嵌套的 .NET monorepo 可调高。 |

---

## 📜 授权与致谢

[PolyForm Noncommercial 1.0.0](../../LICENSE.md)。明确允许个人使用、研究、业余项目，以及非营利组织。**本授权不授予商业使用权**——商业授权请联系上游 GitNexus 作者 Abhigyan Patwari。

构建于：
- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — 原始设计、CLI 命令面与概念模型
- [tree-sitter](https://tree-sitter.github.io/) — 稳健的增量 AST 解析
- [rkyv](https://rkyv.org/) — 零拷贝反序列化框架
- [Tantivy](https://github.com/quickwit-oss/tantivy) — 全文搜索引擎
- [Rayon](https://github.com/rayon-rs/rayon) — 多核并行 AST 解析的数据并行性
- [xxhash (xxh3_64)](https://xxhash.com/) — 用于内容式增量索引的非密码学哈希
- [DashMap](https://github.com/xacrimon/dashmap) — 图谱组装用的并行哈希表
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — 用于亚毫秒级图谱访问的零拷贝内存映射
- [msgspec](https://github.com/jcrist/msgspec) — 进程间通信用的高性能 JSON 序列化

代理上手（URL 引导、Claude Code skill、插件安装）：`docs/skills/ecp-onboard/`。并发不变式与重新验证：`../../scripts/audit/audit-concurrency.sh`。

## 🚦 发布状态

已验证的安装路径：`cargo install --git ...`，从源码构建 `ecp`。发布用安装脚本已内含 checksum 与来源验证流程，但二进制下载路径需要先发布 tag 与 release 资产才能端到端验证。面向代理的上手 skill：[docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md)。辅助式配置流程仍在打磨中。

---

<div align="center">

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)

</div>
