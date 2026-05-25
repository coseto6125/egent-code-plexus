<div align="center">

# `ecp` · EgentCodePlexus

### AI agents के लिए बनाया गया structural code graph — मनुष्यों के लिए नहीं।

*22k फ़ाइलें 2.6 s में indexed · हर query &lt;175 ms में जवाब · ईमानदार unknowns, कभी hallucinated edges नहीं।*

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)
![Cold index 2.6s](https://img.shields.io/badge/cold_index-2.6s%20%2F%2022k%20files-brightgreen)
![Query latency](https://img.shields.io/badge/query-%3C175ms%20cold-blue)
![Languages](https://img.shields.io/badge/languages-31%20parsed-orange)
![License](https://img.shields.io/badge/license-PolyForm%20NC-lightgrey)
![Built with Rust](https://img.shields.io/badge/built_with-Rust-orange?logo=rust)
![Status early release](https://img.shields.io/badge/status-early%20release-yellow)

[English](../../README.md) · [繁體中文](./README_zh-TW.md) · [简体中文](./README_zh-CN.md) · [日本語](./README_ja.md) · [한국어](./README_ko.md) · [Español](./README_es.md) · [Português](./README_pt-BR.md) · [Русский](./README_ru.md) · **हिन्दी**

</div>

---

स्वायत्त coding agents **प्रति task 20–50 structural queries** चलाते हैं। ये सभी queries ऐसे tools पर जाती हैं जो मनुष्यों के लिए बने हैं: IDE sidebars, warm-up की ज़रूरत वाले daemons, पढ़ने के लिए formatted output। यह बेमेल तीन ठोस failure modes के रूप में सामने आता है:

1. **Token की बर्बादी** — एक `grep` dump 400 lines लौटाता है जहाँ agent को केवल 10 symbols चाहिए थे
2. **टूटे refactors** — एक missed caller छूट जाता है क्योंकि resolver ने अनुमान लगाया और गलत निकला
3. **Hallucinated dependencies** — जब static analysis किसी edge तक नहीं पहुँच सकता, तो tool एक बना देता है

`ecp` इन तीनों को खत्म करने के लिए बनाया गया था।

| Failure mode | `ecp` का जवाब |
|---|---|
| Raw search output से context window भर जाना | **TOON / compact JSON** — केवल symbols, lines, और edges; कोई padding नहीं |
| Missed caller, silent downstream breakage | **`impact`** — वास्तविक call और extend edges पर exact blast radius |
| Agent की reasoning में fabricated dependency | **`BlindSpot` records** — typed honest unknowns जिनके चारों ओर agent route कर सके |
| Primary language के बाहर graph अंधा हो जाना | **31 languages** — service code, IaC, SQL, smart contracts एक traversal में |

---

## 🎯 Design सिद्धांत

हर design निर्णय का एक स्रोत है: *receiving agent को वास्तव में क्या चाहिए?*

**Output एक data structure है।** TOON और compact JSON केवल वही लेकर चलते हैं जो agent को अपने अगले निर्णय के लिए चाहिए। कोई prose summaries नहीं। कोई visual chrome नहीं। कोई section headers नहीं जो context budget खाएं। Format defaults पहले से ही अधिकांश LLM prompts के लिए सही विकल्प हैं।

**Stateless। Zero warm-up।** हर invocation एक zero-copy `rkyv` graph file को `mmap` करती है और बाहर निकल जाती है। **~140–170 ms प्रति query, startup सहित।** कोई daemon जिंदा रखने की ज़रूरत नहीं। कोई warm-up phase नहीं। कोई "server crashed, please restart" recovery path नहीं। एक agent प्रति task 50 queries बिना process boot cost चुकाए चला सकता है।

**Hallucination की जगह BlindSpot।** जब `ecp` किसी call site को statically resolve नहीं कर सकता — dynamic dispatch, reflection, एक unresolved import — तो यह एक `BlindSpot` record emit करता है: graph में एक named, typed, explicit gap। Agents किसी known unknown के चारों ओर navigate कर सकते हैं। वे एक confident fabrication से उबर नहीं सकते।

**Polyglot by default।** 31 languages structural depth पर। Service code, Dockerfiles, GitHub Actions, Terraform, SQL, Move, Solidity — एक traversal सभी layers को cover करता है। कोई language switch नहीं मतलब कोई graph blind spot नहीं।

🎙️ **[Agent Interviews](../../interviews/README.md)** — Gemini CLI और Codex बताते हैं कि वे live autonomous task flows में `ecp` का उपयोग कैसे करते हैं।

[Abhigyan Patwari](https://github.com/abhigyanpatwari) के [GitNexus](https://github.com/abhigyanpatwari/GitNexus) पर built — same structural-graph concept, Rust में rewritten, अलग audience। [PolyForm Noncommercial 1.0.0](../../LICENSE.md); required attribution के लिए [NOTICES.md](../../LICENSES/NOTICES.md) देखें।

---

## ⚡ Performance प्रमाण

दो अन्य code-graph tools के साथ सीधी तुलना — [`codegraph`](https://github.com/colbymchenry/codegraph) (Node + SQLite) और upstream [`gitnexus`](https://github.com/abhigyanpatwari/GitNexus) (Node) — समान checkouts, समान machine पर। `ecp` एक stateless one-shot CLI है: नीचे की हर latency में **पूरा process startup शामिल** है, कोई daemon नहीं, कोई warm-up नहीं।

*Versions: `ecp` 0.4.2 · `codegraph` 0.9.4 · `gitnexus` 1.6.5. जहाँ configurable हो वहाँ सभी tools 1 MiB max-file-size threshold पर cap (`gitnexus` 512 KB hard-code करता है)। `ecp` medians 5–7 runs पर। Hardware: AMD Ryzen 9 9950X (16 logical), Linux।*

### `microsoft/vscode` — 14,874 फ़ाइलें, घना single-language TypeScript

| Metric | **`ecp`** | `codegraph` | `gitnexus` |
|---|---|---|---|
| **Cold index** | **4.6 s** | 166.9 s | **DNF** — 27 मिनट में kill |
| Peak RSS | **~1.0 GiB** | 1.7 GiB | 4.6 GiB (अभी भी बढ़ रहा) |
| Symbol find / query | **34.6 ms** | 169.5 ms | — |
| Callers / impact | **27.2 ms** | 172.4 ms | — |
| Inspect / context | **35.0 ms** | 415.9 ms | — |
| Impact baseline (git-diff) | **725.9 ms** | N/A — ऐसा कोई mode नहीं | — |
| Graph nodes | **507,257** | 315,498 | — |
| Graph edges | 916,380 | **986,709** | — |
| Index size on disk | **87 MiB** | 671 MiB | — |
| Files indexed | **14,874** | 10,814 | — |

*`gitnexus` finish नहीं हुआ — in-memory graph-resolution phase में 27 मिनट stuck रहने के बाद kill (RSS 4.6 GiB, कोई output नहीं लिखा)।*

### `abhigyanpatwari/GitNexus` — 3,232 फ़ाइलें, polyglot (वह corpus जो तीनों finish कर सकते हैं)

| Metric | **`ecp`** | `codegraph` | `gitnexus` |
|---|---|---|---|
| **Cold index** | **0.74 s** | 11.2 s | 77.6 s |
| Peak RSS | **264 MiB** | 501 MiB | 2.5 GiB |
| Find / query | **9.4 ms** | 103.5 ms | — |
| Callers / impact | **9.2 ms** | 104.2 ms | 297.6 ms |
| Inspect / context | **9.4 ms** | — | 295.5 ms |
| Graph nodes | **49,122** | 19,604 | 30,223 |
| Graph edges | **48,271** | 39,155 | 47,218 |
| Index size on disk | **7.7 MiB** | 37 MiB | 306 MiB |
| Files indexed | **3,232** | 2,968 | 3,232 |

**Cold index: `codegraph` से 15–37× तेज़; `gitnexus` एक real large repo पर finish नहीं होता। हर scale पर — सबसे कम memory, सबसे छोटा on-disk index, सबसे घना graph।**

### Scale: `.sample_repo` — 22,645 फ़ाइलें, 25 languages, 2.1 GB polyglot corpus

**Ingest:**

| Metric | Value |
|---|---|
| Indexed फ़ाइलें | **22,645** 25 detected languages में |
| Cold ingest | **2.60 s** (parse + resolve + serialize) |
| Incremental ingest | **4.9 ms** (xxh3_64 hash walk, zero dirty files) |
| Hardware | AMD Ryzen 9 9950X (16 logical), 39.2 GiB RAM, Linux 6.6.87 |

**Per-query latency, process startup सहित:**

| Query | Median | यह क्या cover करता है |
|---|---|---|
| `summary` | **1.4 ms** | registry mmap — सबसे छोटा read |
| `routes` | **142.3 ms** | declarative + imperative route enumeration |
| `summary --detailed` | **143.4 ms** | full registry + per-framework confidence scoring |
| `impact --direction down` | **145.0 ms** | Calls / Extends edges पर BFS |
| `inspect` | **145.6 ms** | symbol resolution + 1-hop traversal |
| `find --mode bm25` | **154.5 ms** | Tantivy query + 5-bucket partition |
| `cypher` (narrow) | **161.5 ms** | एक pattern, एक row |
| `cypher` (broad) | **174.2 ms** | wider pattern, अधिक matches |
| `impact --baseline HEAD~1` | **359.0 ms** | git diff + parallel per-file parse + BFS |

सब कुछ reproduce करें: `python scripts/benchmark/benchmark_ecp.py`।

### Rust-tier competitor तुलना

`scripts/benchmark/benchmark_vs_competitors.py` 6 phases में [`codescope`](https://github.com/onur-gokyildiz-bhi/codescope) (SurrealDB-backed) और `coraline` (SQLite-backed) के विरुद्ध benchmark करता है: `cold-index`, `symbol-find`, `callers`, `file-context`, `route-map`, `cypher`। Missing phases → `N/A` (अनुपस्थिति signal है)। Results `docs/benchmark-vs-competitors.md` regenerate करते हैं।

```bash
python scripts/benchmark/benchmark_vs_competitors.py
python scripts/benchmark/benchmark_vs_competitors.py --corpus path/to/repo --iterations 5 --no-plot
```

---

## 🆚 upstream GitNexus से तुलना

Same structural-graph concept, अलग audience। Drop-in replacement नहीं — इस आधार पर चुनें कि output कौन पढ़ता है और उसके साथ क्या करता है।

| Dimension | EgentCodePlexus | GitNexus |
|---|---|---|
| Primary consumer | स्वायत्त AI code agents | Human devs + IDE integration |
| Runtime | Stateless one-shot CLI (zero warm-up) | Long-running MCP server |
| Performance | **< 2.5s cold index / < 175ms query** | ~60s cold index / ~400ms query |
| Unresolved edge | `BlindSpot` record (honest unknown) | Heuristic guess |
| Default output | TOON / compact JSON (token-cheap) | Wiki / UI rendering |
| Languages | 31 (14 deep + 17 structural) | 14 (deep, 9-dimension) |
| Storage | Rust + `rkyv` zero-copy mmap | Node.js + LadybugDB |

**पूरा breakdown, philosophy, और decision matrix → [docs/vs-gitnexus.md](../vs-gitnexus.md)**

---

## 📦 Install

Prebuilt binaries हर GitHub Release के साथ आती हैं। Installer scripts केवल तभी cargo source build पर fallback करती हैं जब matching asset उपलब्ध नहीं होता।

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# Direct cargo (कोई installer wrapper नहीं)
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

CPU-tuned source build:

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

---

## 🚀 Quick start

कोई daemon start नहीं करना। कोई config की ज़रूरत नहीं। शून्य से queryable graph तक एक command।

```bash
# Index करें (incremental; अगर index अनुपस्थित है तो पहली query भी auto-index करती है)
ecp admin index --repo .

# Symbol ढूंढें — default रूप से exact
ecp find loginUser
ecp find login --mode bm25            # BM25 ranking, 5 output buckets में विभाजित

# Blast radius — अगर मैं यह बदलूँ तो क्या टूटेगा?
ecp impact validateUser --direction upstream

# पूरा symbol context (signature, body, callers, callees, 1-hop impact)
ecp inspect validateUser

# HTTP route map (declarative @Get + imperative app.get())
ecp routes
ecp routes /api/users --method POST   # route → handler → caller chain

# File usage: इस path को कौन read / write करता है?
ecp impact --literal session_meta.json
```

सभी read-side commands `--format text|json|toon` स्वीकार करते हैं। Defaults प्रति command token-cheapest हैं (अधिकतर `toon`; `find` defaults to `text`; `cypher`/`summary` default to `json`)।

---

## 🛠️ CLI surface

दो tiers: top level पर **agent commands** (query / refactor / verify) और `ecp admin` के अंतर्गत **admin commands** (registry / hooks / destructive)। पूरे flag matrices के लिए `ecp --help` और `ecp admin --help` चलाएं।

**Agent commands:**

| Command | Purpose |
|---|---|
| `inspect <name>` | Symbol → metadata, decorators, signature, callers, callees, 1-hop impact, contained methods / properties / enum variants |
| `find <pattern>` | Exact · `--mode fuzzy` · `--mode bm25` (5 buckets: source / tests / reference / document / config) |
| `find-schema-bindings <field>` | MirrorsField heuristic edges + blind-spot candidates across classes / services |
| `find-transaction-patterns [--class <Name>]` | Saga compensate/undo/rollback name-pairs; ≥0.75 → POSSIBLY_RELATED, <0.75 → BLIND_SPOT |
| `impact <name> --direction <up\|down>` | Blast-radius BFS with confidence filtering; `--since <ref>` for change-set impact |
| `rename --symbol <old> --new-name <new>` | 14 languages में AST-aware multi-file rename। हमेशा पहले `--dry-run` करें। |
| `cypher '<query>'` | openCypher escape hatch; `m.content` source body लौटाता है |
| `summary` | Registry overview, framework coverage, LLM-actionable blind-spot catalog, graph freshness |
| `routes [<path>]` | HTTP route enumeration (declarative + imperative); `<path>` के साथ: handler + caller chain |
| `contracts` | Cross-repo API contract inventory (routes / queue / RPC) |
| `diff` | Resolver-delta: binding tier-degradation + route / contract changes |
| `tool-map` | Import-binding analysis के माध्यम से external HTTP / DB / Redis / queue call sites |
| `shape-check` | HTTP consumer access patterns और Route response shapes के बीच drift |
| `peers` | Multi-session collaboration: `status / diff / say / inbox / log / thread / watch / gc` |
| `review` | One-shot audit: impact + summary + tool-map + shape-check + diff, केवल high-confidence signals |

**Admin commands** (`ecp admin <cmd>`):

| Command | Purpose |
|---|---|
| `index --repo <path>` | Graph build / refresh करें; xxh3_64 content cache के माध्यम से incremental। Full rebuild के लिए `--force`। |
| `drop / prune / rename-branch` | Index lifecycle: delete, stale branch dirs prune करें, on-disk branch rename करें |
| `install-hook` | Git reference-transaction hook (branch switches auto-track करता है) |
| `config` | `.ecp/config.toml` के लिए interactive TOML wizard |
| `mcp serve` / `mcp tools` | MCP server (stdio); `tools` exposed surface list करता है |

सभी commands CWD से `.ecp/graph.bin` resolve करते हैं जब तक `--graph <path>` न दिया जाए। हर agent-facing command non-interactive है; हर output stream parseable है।

### Multi-session peer sync

जब multiple LLM sessions एक ही repo को parallel में edit करती हैं, तो `ecp peers` हर session की symbol-level dirty state surface करता है और direct session messaging enable करता है। `ECP_SESSION_ID`, `CODEX_SESSION_ID`, `CODEX_THREAD_ID`, या `CLAUDE_CODE_SESSION_ID` के माध्यम से register करें।

```bash
# Watcher start करें (प्रति session एक; inbox push events के लिए ज़रूरी)
ecp peers watch --start

# अभी और कौन edit कर रहा है?
ecp peers status                                  # text
ecp peers status --format json                    # {session_id, pid, watcher: alive|dead|not-started}

# किसी peer के dirty symbols inspect करें
ecp peers diff <peer-session-id> [<symbol>]

# Messages भेजें
ecp peers say "rebasing on main, hold pushes 5min"    # broadcast
ecp peers say --to <peer-session-id> "take auth.rs?"  # targeted

# Read और manage करें
ecp peers inbox
ecp peers log --limit 20
ecp peers thread <msg-id>

# Cleanup
ecp peers watch --stop && ecp peers gc
```

`watcher` field `alive` | `dead` | `not-started` में अंतर करता है — crashes "feature not used" के रूप में नहीं छुपते।

### Provable code-review verdicts

`ecp review --verdicts` `ecp diff` sections से graph-backed verdicts pre-compute करता है। JSON को सीधे review context के रूप में pass करें — एक raw diff से caller relationships की LLM re-derivation skip करें।

```bash
ecp review --since main --verdicts --format json
```

| Severity | Rule |
|---|---|
| `RISK` | Cross-file callers exist, public symbol removed, या diff region में blindspot |
| `WARN` | केवल intra-file callers, या route modified |
| `INFO` | कोई callers नहीं मिले, या new public surface added |

Verdict kinds: `SIGNATURE_OR_BODY_CHANGED` · `NEW_PUBLIC_SURFACE` · `REMOVED_PUBLIC_SURFACE` · `ROUTE_CONTRACT_CHANGED` · `BLINDSPOT_IN_DIFF_REGION`

हर verdict उस exact diff section और graph fact को cite करता है जिसने उसे trigger किया। पूरा spec: [docs/specs/2026-05-22-review-verdicts.md](../specs/2026-05-22-review-verdicts.md)।

---

## 🔌 Agent integration

**जहाँ उपलब्ध हो native path prefer करें** — यह auto-reindex hooks और workflow skills wire करता है जो agent को सिखाता है कि graph queries कब round-trip के लायक हैं। **MCP universal fallback है** किसी भी host के लिए जो protocol बोलता है।

| Agent | Path | Wires |
|---|---|---|
| Claude Code | native | hooks + skills + optional MCP |
| Codex CLI | native | skills (native-tools pending) |
| Gemini CLI | native | native skill **या** MCP |
| Cursor · Windsurf · Cline · Copilot · any MCP host | MCP | MCP server |

Guided setup: `ecp admin → Agent Integrations → <host>`। Automation के लिए scriptable path: `ecp admin <host> install <component>`। किसी host को inspect करें: `ecp admin <host> status`।

### Claude Code

```bash
ecp admin claude install hooks          # settings.json: auto-reindex + context enrichment
ecp admin claude install skills all     # ecp + simplify skill packs (या: ecp | simplify)
ecp admin claude install mcp-server     # optional — hooks + skills + CLI already sufficient
```

Hooks बिना explicit tool call के हर Grep/Glob/Bash पर graph context feed करते हैं। `ecp` skill symbol / impact / route / contract / rename workflows सिखाता है। `simplify` graph-first code review चलाता है।

### Gemini CLI

```bash
ecp admin gemini install native-skill   # `gemini skills link` के माध्यम से links
ecp admin gemini install mcp-server     # `gemini mcp add` के माध्यम से registers
```

`native-skill` और `mcp-server` परस्पर exclusive हैं — एक install करने से दूसरा remove हो जाता है।

### Codex CLI

```bash
ecp admin codex install skills all      # ecp + simplify; native-tools Codex wiring pending
```

**Workflow skills:**

| Skill | कब उपयोग करें |
|---|---|
| `ecp` | Agent तय करता है कि graph-aware workflows symbols, callers, routes, contracts के लिए grep / file reads से बेहतर हैं या नहीं |
| `simplify` | ecp impact, blind spots, egress, shape drift, resolver deltas से शुरू होने वाला Code review |

### MCP fallback (Cursor, Windsurf, Cline, any MCP host)

| Host | Config file |
|---|---|
| Cursor | `~/.cursor/mcp.json` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` |
| Cline (VS Code) | `cline_mcp_settings.json` (MCP panel → "Edit MCP Settings") |
| Generic MCP host | host-specific |

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

```bash
ecp admin mcp tools    # connect करने से पहले exposed surface verify करें
ecp admin mcp serve    # प्रति call stateless one-shot (कोई warm-up cost नहीं)
```

---

## 🏗️ Architecture

```
crates/
├── ecp-core        # Zero-copy graph (rkyv + mmap), incremental cache, graph queries
├── ecp-analyzer    # Tree-sitter parsers, HTTP route detector, framework confidence
├── ecp-mcp         # MCP server (stdio) — core commands को tools के रूप में expose करता है
└── ecp-cli         # `ecp` binary, Tantivy BM25 engine, token-optimized output
```

Parse → resolve → serialize एक MPSC channel के माध्यम से एक single builder thread में चलता है जो graph assemble करता है और एक zero-copy `.ecp/graph.bin` लिखता है। Read paths (`inspect`, `cypher`, `impact`, …) इस file को सीधे mmap करते हैं — कोई deserialization step नहीं। xxh3_64 content cache 22k-file repo पर incremental rebuilds को sub-second रखता है।

---

## 🌐 Language coverage

31 languages structural level पर parsed। **14 full-depth** (TypeScript, JavaScript, Python, Java, Kotlin, C#, Go, Rust, PHP, Ruby, Swift, C, C++, Dart) — imports, named bindings, exports, heritage, types, constructors, config, frameworks, entry points, calls, और rename। **17 structural-only**: Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig।

📊 **[पूरा Language Capability Matrix](../language-matrix.md)** — per-language status और rationale।

---

## ⚙️ Tuning

| Env var | Default | Effect |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216` (16 MiB) | Ingest के दौरान इस size से बड़ी source files को skip करें। Worst-case worker RAM को `num_threads × MAX` पर cap करता है। |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | `*.csproj` discovery recursion depth। Deeply-nested .NET monorepos के लिए बढ़ाएं। |

---

## 📜 License और acknowledgments

[PolyForm Noncommercial 1.0.0](../../LICENSE.md)। Personal use, research, hobby projects, और noncommercial organizations को स्पष्ट रूप से permitted। **Commercial use इस license द्वारा granted नहीं है** — commercial rights के लिए upstream GitNexus author Abhigyan Patwari से संपर्क करें।

Built on:
- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — original design, CLI surface, और conceptual model
- [tree-sitter](https://tree-sitter.github.io/) — robust incremental AST parsing
- [rkyv](https://rkyv.org/) — zero-copy deserialization framework
- [Tantivy](https://github.com/quickwit-oss/tantivy) — full-text search engine
- [Rayon](https://github.com/rayon-rs/rayon) — multi-core concurrent AST parsing के लिए data parallelism
- [xxhash (xxh3_64)](https://xxhash.com/) — content-based incremental indexing के लिए non-cryptographic hashing
- [DashMap](https://github.com/xacrimon/dashmap) — graph assembly के लिए concurrent hash maps
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — sub-millisecond graph access के लिए zero-copy memory mapping
- [msgspec](https://github.com/jcrist/msgspec) — inter-process communication के लिए high-performance JSON serialization

Agent onboarding (URL bootstrap, Claude Code skill, plugin install): `docs/skills/ecp-onboard/`। Concurrency invariants और re-verification: `../../scripts/audit/audit-concurrency.sh`।

## 🚦 Release status

Verified install path: `cargo install --git ...`, जो `ecp` को source से build करता है। Release installers में पहले से checksum और provenance-verification flow शामिल है, लेकिन binary download path end-to-end verified होने के लिए published tag और release assets की ज़रूरत है। Agent-facing onboarding skill: [docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md)। Assisted configuration/setup flow अभी भी refined हो रहा है।

---

<div align="center">

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)

</div>
