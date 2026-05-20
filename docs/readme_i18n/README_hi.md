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

[English](../../README.md) · [繁體中文](./README_zh-TW.md) · [简体中文](./README_zh-CN.md) · [Español](./README_es.md) · [Русский](./README_ru.md) · **हिन्दी** · [日本語](./README_ja.md) · [한국어](./README_ko.md) · [Português (BR)](./README_pt-BR.md)

---

## ── कारण ──

Code agents एक task में 20–50 बार lookup करते हैं। `grep` केवल strings लौटाता है; एक autonomous agent को चाहिए symbols, callers, edges, और एक ईमानदार signal जब static graph जवाब नहीं दे सकता।

`ecp` वह structural knowledge layer है जो:

- **stateless है।** हर invocation एक zero-copy `rkyv` graph को `mmap` करता है और exit हो जाता है। कोई daemon गर्म नहीं रखना है, कोई "server died, please restart" failure mode नहीं है।
- **ईमानदार है।** जब call site statically resolve नहीं हो सकता (dynamic dispatch, unresolved import, reflection), `ecp` एक `BlindSpot` record emit करता है। एक agent जो hallucinated dependency पर action लेता है, उससे महंगा पड़ता है जो "मुझे नहीं पता" पाकर रास्ता बदल लेता है।
- **token-cheap है।** Default output TOON है (compact key:value)। हर flag `--help` के ज़रिए मिलता है। हर command non-interactive है और इसका `stdout` parseable है। कोई UI clutter context window को नहीं खाता।
- **polyglot है।** 31 languages को structural level पर parse करता है — service code, Dockerfile, GitHub Actions, Terraform, SQL और smart contracts जैसे ही main language से बाहर जाते हैं, वे black hole बनना बंद कर देते हैं।

[Abhigyan Patwari](https://github.com/abhigyanpatwari) के [GitNexus](https://github.com/abhigyanpatwari/GitNexus) पर बनाया गया — वही conceptual model, अलग audience के लिए Rust में फिर से लिखा गया।

🎙️ **[Agent interviews](../../interviews/README.md)** — Gemini CLI और Codex autonomous workflows में `ecp` का evaluation करते हैं।

---

## ── आँकड़े ──

upstream GitNexus के विरुद्ध head-to-head, [gitnexus](https://github.com/abhigyanpatwari/GitNexus) codebase (TypeScript) पर `scripts/parity/benchmark_vs_gitnexus.py` से मापा गया:

| Phase | ecp (Rust) | gitnexus (Node) | Speedup |
|---|---|---|---|
| **Cold Index** | **~970 ms** | ~58 s | **60×** |
| **Symbol Context** | **~70 ms** | ~430 ms | **6×** |
| **Blast Radius** | **~70 ms** | ~460 ms | **6×** |
| **Cypher Query** | **~70 ms** | ~400 ms | **5×** |

`ecp` के numbers में पूरा process startup शामिल है (no daemon)। GitNexus (v1.6.5) के numbers warm + indexed repo पर CLI के ज़रिए मापे गए हैं।

<details>
<summary><b>Scalability — <code>.sample_repo</code> पर एकल run</b> (2.1 GB polyglot, ~40 OSS projects, 25+ languages)</summary>

**Ingest performance**

| Phase | Value |
|---|---|
| Files indexed | **22,645** across 25 detected languages |
| Wall-clock (Cold) | **2.60 s** (parse + resolve + serialize) |
| Wall-clock (Incremental) | **4.9 ms** (xxh3_64 hash walk, zero dirty files) |
| Hardware | AMD Ryzen 9 9950X (16 logical), 39.2 GiB RAM, Linux 6.6.87 |

**Per-query latency** (process startup सहित)

| Query | Median | Notes |
|---|---|---|
| `coverage` (registry overview) | **1.4 ms** | सबसे छोटा read — सिर्फ़ registry mmap |
| `routes` (पूरे repo पर HTTP route map) | **142.3 ms** | declarative + imperative दोनों enumerate करता है |
| `coverage --detailed` (frameworks + blind-spots) | **143.4 ms** | पूरा registry + per-framework scoring |
| `impact <symbol> --direction down` | **145.0 ms** | Calls / Extends edges पर BFS |
| `inspect <symbol>` (signature + callers + callees) | **145.6 ms** | symbol resolution + 1-hop traversal |
| `find <name> --mode bm25` (lexical search) | **154.5 ms** | Tantivy query + 5-bucket partition |
| `cypher 'MATCH (a:Class)-[:HasMethod]->(b:Method) ...'` | **161.5 ms** | एक pattern, एक row |
| `cypher 'MATCH (a:Method)-[:Calls]->(b:Method) ...'` | **174.2 ms** | broader pattern, अधिक matches |
| `impact --baseline HEAD~1` (changeset blast radius) | **359.0 ms** | git diff + parallel per-file parse + BFS |

Reproduce: `python scripts/benchmark/benchmark_ecp.py`.

</details>

---

## ── vs. upstream gitnexus ──

वही conceptual model, अलग audience। `ecp` एक drop-in replacement **नहीं है** — चुनाव इस आधार पर करें कि graph को कौन पढ़ रहा है।

| Dimension | EgentCodePlexus | GitNexus |
|---|---|---|
| Primary consumer | Autonomous AI code agents | Human devs + IDE integration |
| Runtime | Stateless one-shot CLI (zero warm-up) | Long-running MCP server |
| Performance | **< 2.5 s cold index / < 150 ms query** | ~60 s cold index / ~400 ms query |
| Unresolved edge | `BlindSpot` record (ईमानदार unknown) | Heuristic guess |
| Default output | TOON / compact JSON (token-cheap) | Wiki / UI rendering |
| Languages | 31 (14 deep + 17 structural) | 14 (deep, 9-dimension) |
| Storage | Rust + `rkyv` zero-copy mmap | Node.js + LadybugDB |

8 dimensions का पूरा breakdown + decision matrix → [docs/vs-gitnexus.md](../vs-gitnexus.md)।

---

## ── 30-second demo ──

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

यही पूरा round-trip है — एक process, एक mmap, ~140 ms। Read-side commands `--format text|json|toon` स्वीकार करते हैं; per-command default वह encoding है जो tokens में सबसे सस्ती हो।

---

## ── Install ──

हर GitHub Release के साथ prebuilt binaries publish होते हैं। Installer scripts cargo source build पर तभी fall back करते हैं जब matching release asset उपलब्ध नहीं है।

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# Explicit cargo path (वही source build, बिना installer wrapper)
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

## ── Quick start ──

```bash
# 1. वर्तमान repo को index करें (incremental; पहली query auto-index भी करती है)
ecp admin index --repo .

# 2. एक symbol locate करें — default में exact name
ecp find loginUser
ecp find login --mode bm25       # BM25 ranking, top-K source/tests/ref/doc/config buckets में

# 3. Blast radius — इसे बदलूँ तो कौन टूटेगा?
ecp impact validateUser --direction upstream

# 4. Symbol का पूरा context (signature, body, callers, callees, 1-hop impact)
ecp inspect validateUser

# 5. Repo के सारे HTTP routes (declarative @Get + imperative app.get())
ecp routes
ecp routes /api/users --method POST     # route → handler → caller chain
```

---

## ── CLI surface ──

दो tiers — **agent commands** top level पर (query / refactor / verify) और **admin commands** `ecp admin` के अंतर्गत (registry / hooks / destructive)। पूरी flag matrix के लिए `ecp --help` और `ecp admin --help` चलाएँ।

| Command | Purpose |
|---|---|
| `inspect <name>` | एक symbol → metadata, decorators, signature, callers, callees, 1-hop impact |
| `find <pattern>` | Symbols locate करें — exact (default) · `--mode fuzzy` substring · `--mode bm25` lexical ranking; bm25 output को source / tests / reference / document / config buckets में partition करता है |
| `impact <name> --direction <up\|down>` | Confidence filtering के साथ blast-radius traversal। `--baseline <ref>` changeset impact के लिए। |
| `rename --symbol <old> --new-name <new>` | 14 languages पर AST-aware multi-file rename। हमेशा `--dry-run` पहले। |
| `cypher '<query>'` | openCypher escape hatch; `m.content` source body लौटाता है। |
| `coverage` | Registry overview, framework coverage, blind-spot catalog, graph freshness। |
| `routes [<path>]` | HTTP routes enumerate करें (declarative + imperative); `<path>` दें तो handler + callers दिखाता है। |
| `contracts` | Cross-repo API contract inventory (routes / queue / RPC)। |
| `diff` | Resolver-delta — edge-level binding tier-degradation + route / contract changes। |
| `tool-map` | External HTTP / DB / Redis / queue clients पर calls — per-file import-binding analysis से। |
| `shape-check` | HTTP consumer access patterns और Route response shapes के बीच drift। |
| `peers` | Multi-session peer collaboration (status / diff / log / gc)। |
| `review` | LLM-workflow audit aggregator — impact + coverage + tool-map + shape-check + diff, high-confidence signals तक filter। |

<details>
<summary><b>Admin namespace</b> — <code>ecp admin &lt;cmd&gt;</code> (registry / hooks / destructive)</summary>

| Command | Purpose |
|---|---|
| `index --repo <path>` | Graph build / refresh करें; xxh3_64 content cache के ज़रिए incremental। पूरे rebuild के लिए `--force`। |
| `drop / prune / rename-branch` | Index lifecycle: delete, stale branch dirs prune, on-disk branch rename। |
| `install-hook` | git reference-transaction hook install (branch switches auto-track)। |
| `config` | `.ecp/config.toml` के लिए interactive TOML wizard। |
| `mcp serve` / `mcp tools` | LLM hosts के लिए MCP server (stdio); `tools` exposed tool surface list करता है। |
| `claude install / codex install / gemini install` | Scriptable host integration (skills, hooks, MCP entries)। |
| `verify-resolver` | Resolver dump को language oracle से diff (ecp-dev QA)। |

</details>

जब तक `--graph <path>` न दिया जाए, सभी commands CWD से `.ecp/graph.bin` resolve करते हैं। Agent-facing commands design से non-interactive हैं — हर flag `--help` से, हर output stream parseable। `ecp admin` बिना subcommand चलाने पर interactive admin TUI खुलता है।

---

## ── MCP server ──

`ecp` एक MCP server ship करता है जो core commands को MCP tools के रूप में expose करता है। MCP बोलने वाले hosts (Claude Code, Cursor, Windsurf, Cline, Codex CLI, Gemini CLI) `ecp` को register करके autonomously tools call कर सकते हैं।

```bash
ecp admin mcp tools          # देखें कौन-से tools expose होंगे
ecp admin mcp serve          # server चलाएँ (default में spawn mode)
```

Claude Code के लिए manual host config example (`~/.config/claude-code/mcp-servers.json`):

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

Human operators के लिए progressive path:

```text
ecp admin → Agent Integrations → MCP → <host> → install
```

AI agents के लिए scripted path:

```bash
ecp admin claude install mcp-server
ecp admin gemini install skills
```

<details>
<summary><b>Codex CLI native integration</b> (MCP से अलग — openai/codex fork के लिए patch तैयार करता है)</summary>

Codex native path आपके चल रहे Codex installation को edit नहीं करता; यह एक patch लिखता है जिसे आप `openai/codex` fork पर apply करते हैं।

Progressive path:

```text
ecp admin → Agent Integrations → Codex CLI → install → native-tools
```

Bundled skills (वही progressive path):

```text
ecp admin → Agent Integrations → Codex CLI → install → skills → all | ecp | simplify
```

Agents के लिए scripted path:

```bash
ecp admin codex install native-tools
ecp admin codex install skills all
ecp admin codex install skills ecp
ecp admin codex install skills simplify
```

Bundled skills वह workflow selection सिखाते हैं जो command help अकेले infer नहीं कर सकता:

| Skill | कब उपयोग करें |
|---|---|
| `ecp` | Agent को decide करना है कि symbol / impact / route / contract / rename के graph-aware workflows grep / file reads से बेहतर हैं या नहीं। |
| `simplify` | Agent बदले हुए code की review कर रहा है और raw diff पढ़ने से पहले `ecp impact`, blind spots, egress, shape drift, और resolver deltas से शुरू करना चाहिए। |

`native-tools` component लिखता है:

```text
~/.config/ecp/host-integration/codex-cli.patch
```

अपने Codex CLI fork पर apply करें:

```bash
cd /path/to/openai-codex-fork
git apply ~/.config/ecp/host-integration/codex-cli.patch
```

जिस fork पर पहले से native marker है, उसकी verify करने के लिए — status check से पहले `ECP_CODEX_CLI_CHECKOUT` set करें:

```bash
ECP_CODEX_CLI_CHECKOUT=/path/to/openai-codex-fork ecp admin codex status
ecp admin codex uninstall native-tools
ecp admin codex uninstall skills all
```

</details>

---

## ── Architecture ──

```
crates/
├── ecp-core        Zero-copy graph (rkyv + mmap), incremental cache, graph queries
├── ecp-analyzer    Tree-sitter parsers, HTTP route detector, framework confidence
├── ecp-mcp         MCP server (stdio) — core commands को tools के रूप में expose करता है
└── ecp-cli         `ecp` binary, Tantivy BM25 engine, token-optimized output
```

Parse → resolve → serialize एक MPSC channel से होकर एक single builder thread तक जाता है जो graph assemble करता है और एक zero-copy `.ecp/graph.bin` लिखता है। Read paths (`inspect`, `cypher`, `impact`, …) इस file को सीधे mmap करते हैं। xxh3_64 content cache 22k-file repo पर भी incremental rebuilds को sub-second में रखता है।

---

## ── Language coverage ──

31 languages structural level पर parse होती हैं (functions / classes / methods / imports / calls)। उनमें से 14 — original GitNexus set — को imports, named bindings, exports, heritage, types, constructors, config, frameworks, entry points, calls और rename के पार full-depth coverage मिलती है। बाकी 17 structural-only हैं (Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig)।

📊 [Full Language Capability Matrix](../language-matrix.md) — per-language status और rationale।

---

## ── Tuning ──

| Env var | Default | Effect |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216` (16 MiB) | Ingest के दौरान इससे बड़ी source files skip करता है। Worst-case worker RAM को `num_threads × MAX` पर रोकता है। |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | `*.csproj` discovery के लिए directory recursion depth। गहरे-nested .NET monorepos के लिए बढ़ाएँ। |

---

## ── License ──

[PolyForm Noncommercial 1.0.0](../../LICENSE.md) के तहत licensed। Personal use, research, hobby projects, और noncommercial organizations को स्पष्ट रूप से अनुमति है। **Commercial use इस license से नहीं मिलता** — commercial rights के लिए upstream GitNexus लेखक [Abhigyan Patwari](https://github.com/abhigyanpatwari) से contact करें। आवश्यक attribution: [NOTICES.md](../../LICENSES/NOTICES.md)।

<details>
<summary><b>Built on</b> (acknowledgments)</summary>

- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — original design, CLI surface, conceptual model
- [tree-sitter](https://tree-sitter.github.io/) — incremental AST parsing
- [rkyv](https://rkyv.org/) — zero-copy deserialization framework
- [Tantivy](https://github.com/quickwit-oss/tantivy) — Rust BM25 search engine
- [Rayon](https://github.com/rayon-rs/rayon) — multi-core AST parsing के लिए data parallelism
- [xxhash (xxh3_64)](https://xxhash.com/) — content-based incremental indexing के लिए hashing
- [DashMap](https://github.com/xacrimon/dashmap) — graph assembly के लिए concurrent hash maps
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — zero-copy memory mapping
- [msgspec](https://github.com/jcrist/msgspec) — IPC के लिए fast JSON serialization

AI agents के लिए onboarding (URL bootstrap, Claude Code skill, plugin install) `docs/skills/ecp-onboard/` में है। Concurrency invariants और उन्हें re-verify करने का तरीका: `./scripts/audit/audit-concurrency.sh`।

</details>

---

## ── Release status ──

वर्तमान verified install path `cargo install --git ...` है, जो `ecp` को source से build करता है। Release installers में checksum और provenance-verification flow पहले से है, लेकिन binary download path को end-to-end verify करने के लिए published tag और release assets ज़रूरी हैं। Agent-facing onboarding skill [docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md) पर documented है; यह users को install, first index, optional groups, MCP wiring और next steps के through ले जाता है — assisted setup flow अभी refine हो रहा है।

---

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)
