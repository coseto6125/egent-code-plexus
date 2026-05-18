# Graph Nexus for LLM

A code intelligence graph built for **LLMs and AI code agents** — not for human IDE integration. Indexes any codebase in milliseconds, then answers structural questions like *who calls this*, *what's the blast radius if I change this function*, or *what's related to the auth flow*.

Built on top of [GitNexus](https://github.com/abhigyanpatwari/GitNexus) by [Abhigyan Patwari](https://github.com/abhigyanpatwari) — same core idea (a structural knowledge graph of a repo), rewritten in Rust for a different audience. Licensed under [PolyForm Noncommercial 1.0.0](./LICENSE).

> Required Notice: Copyright Abhigyan Patwari (https://github.com/abhigyanpatwari/GitNexus). Not affiliated with or endorsed by the upstream GitNexus project. Noncommercial use only. See [NOTICES.md](./NOTICES.md) for the full third-party attribution list.

## vs. upstream GitNexus

> **Not a drop-in replacement.** Upstream is a broader Node/TypeScript agent platform (MCP server, resources, hooks, generated skills); graph-nexus is a stateless Rust CLI optimized for shell-mediated LLM calls — different scope, different tradeoffs.

| Dimension | GitNexus | graph-nexus | Why it matters for an LLM agent |
|---|---|---|---|
| **Audience** | Human devs + IDE integration | AI code agents | Optimisation target shapes every other row |
| **Runtime** | Long-running MCP server | One-shot CLI, rkyv mmap zero-copy | Sub-second per query; an agent can fire 30+ queries per task with no warm-up cost |
| **Unresolved import** | Heuristic guess (e.g. Jaccard) to keep the graph readable | `BlindSpot` record, no edge — never fabricate | Agent never acts on a hallucinated dependency; an honest "I don't know" beats a confident wrong answer |
| **Output format** | Wiki / UI rendering | `etoon` / `cypher` / compact JSON | No UI cruft eating context window; tokens spent on graph, not on layout |
| **Languages parsed** | 14 (TypeScript, JavaScript, Python, Java, Kotlin, C#, Go, Rust, PHP, Ruby, Swift, C, C++, Dart) | 31 — same 14 plus Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig | Mixed-stack repos (DevOps configs, Web3 contracts, infra-as-code) stop being black holes |

> Language depth varies. graph-nexus parses 31 languages at the structural level (functions / classes / methods / imports); it does not yet match GitNexus's full 9-dimension coverage (Named Bindings, Heritage, Constructor Inference, Config, ...) on every language. Treat the 31 count as breadth, not parity — see [Language Matrix](#language-matrix) below.

### Tool & integration coverage

| LLM-facing area | Upstream GitNexus (`._source_code`) | Graph Nexus Rust (`gnx`) |
| :--- | :--- | :--- |
| **Agent integration** | MCP server, resources, prompts, setup, hooks, generated skills | Stateless CLI; use through shell/tool wrappers. **No built-in MCP server yet.** |
| **Core query tools** | `query`, `context`, `impact`, `detect_changes`, `rename`, `cypher`, group tools | `inspect`, `search`, `impact`, `routes`, `cypher`, `coverage`, `rename` (agent); `admin index/drop/prune/…` (admin) |
| **Context output** | Rich MCP responses and generated repo skills | Compact `toon`/JSON/text for shell-mediated LLM calls |
| **Search** | Documented BM25 + semantic + RRF hybrid search | Tantivy BM25 (substring fallback when index absent) |
| **Runtime/storage** | Node.js + LadybugDB | Rust + mmap `rkyv` graph file |
| **Best fit** | Agent runtimes with strong MCP/editor integration | Local LLM harnesses/scripts that want a small executable with few moving parts |

Under the hood: zero-copy on-disk storage (rkyv + mmap), BM25 lexical search via Tantivy, framework-aware route extraction. The CLI is `gnx`.

[繁體中文說明 (Traditional Chinese)](./README_zh-TW.md)

## 🚀 Key Features

*   **Blazing Fast & Zero-Copy**: cold-indexed `.sample_repo` — **22,772 files across 25 detected languages in 4.9 s** (Java 3535, PHP 2907, TypeScript 1704, C# 945, Rust 870, C 801, Markdown 783, Dart 616, Bash 487, C++ 476, JavaScript 466, Solidity 403, Move 367, YAML 343, Ruby 156, Python 134, Swift 105, Go 99, Crystal 72, Kotlin 49, Lua 32, Zig 31, Dockerfile 20, Docker Compose 8, SQL 4). Per-query latency on the same graph: cypher 9 ms · context 9 ms · impact 5–6 ms · route-map 13 ms · BM25 query 24 ms · summarize 38 ms · detect-changes 230 ms. Hardware: **AMD Ryzen 9 9950X (8 vCPU under WSL2, 11.7 GiB RAM)**, Linux 6.6.87. Tree-sitter + Rayon for parse, `rkyv` mmap for zero-copy `graph.bin`. Reproduce: `python scripts/benchmark_gnx.py`.
*   **LLM-Native Output**: Emits extreme token-efficient formats ([TOON](https://crates.io/crates/etoon)) and concise string summaries. No hallucination-inducing formatting.
*   **Lexical Search**: **Tantivy (BM25)** for zero-latency, full-text tokenized keyword matching across the entire indexed corpus, with a per-name substring fallback (1.0 exact / 0.7 prefix / 0.4 contains) so freshly-cloned repos still produce shaped output before the first index materialises.
*   **Incremental Caching**: Only re-computes ASTs for modified files (SHA-256 Content Hash). Graph rebuilds drop from ~50s (Cold Start) to **< 0.25s**!
*   **Zero-Maintenance Route Extraction**: Purely based on RFC 7231 HTTP constants. Extracts API routes from both Declarative (e.g., `@Get`) and Imperative (e.g., `app.get()`) definitions across all languages.
*   **RAG Document Indexing**: Securely isolates `.md` (Markdown) and `.yaml` (GitHub Actions) files into parallel structures, parsing sections natively for LLM documentation retrieval without polluting the code execution graph.

## 📦 Installation

> **Pre-release status**: until the first GitHub Release lands, the prebuilt installer scripts will auto-fallback to `cargo install --git`. Every platform below has at least one working terminal install path right now.

### Every platform (works today, no Release required)

```bash
cargo install --git https://github.com/coseto6125/graph-nexus graph-nexus --bin gnx --locked
```

Needs a Rust toolchain ([rustup.rs](https://rustup.rs)). Source build — first compile takes a few minutes, cached afterwards.

**Optimized for your CPU (recommended for personal install)**:

```bash
RUSTFLAGS="-C target-cpu=native" \
  cargo install --git https://github.com/coseto6125/graph-nexus graph-nexus \
  --bin gnx --locked --profile release-dist
```

`release-dist` enables fat LTO + single codegen unit (slower build, faster runtime). `target-cpu=native` lets the compiler use this machine's full ISA (AVX2/AVX-512/NEON variants) — the resulting binary will only run on CPUs with the same feature set, which is fine for a self-install.

### Per-platform one-liners

| Platform | Command | Notes |
| :--- | :--- | :--- |
| **Linux / macOS** | `curl -sSfL https://raw.githubusercontent.com/coseto6125/graph-nexus/main/install.sh \| sh` | Tries prebuilt Release first; auto-falls back to `cargo install --git` when no Release is published yet. Defaults to `~/.local/bin/gnx`; override with `GNX_INSTALL_DIR=~/bin curl ... \| sh` to land anywhere you control. `GNX_FORCE_CARGO=1` skips the Release lookup. |
| **Windows PowerShell** | `iwr https://raw.githubusercontent.com/coseto6125/graph-nexus/main/install.ps1 -UseBasicParsing \| iex` | Same Release-first / cargo-fallback logic. Defaults to `%LOCALAPPDATA%\Programs\gnx\gnx.exe`; override with `$env:GNX_INSTALL_DIR=...` before piping to `iex`. `$env:GNX_FORCE_CARGO='1'` forces cargo. |
| **macOS Homebrew** | `brew tap coseto6125/tap && brew install graph-nexus` | Available *after* the tap formula is published with the first Release. |
| **Manual** | Download from [GitHub Releases](https://github.com/coseto6125/graph-nexus/releases) | Pick the archive for your target and verify `.sha256`. |

> After install, the binary is named `gnx` (the package on crates.io will be `graph-nexus` once published). `cargo install graph-nexus` from crates.io is intentionally not listed yet: publish is blocked until all analyzer grammar dependencies are available as publishable crate dependencies.

> Once a tagged Release exists, the installer scripts will be served from `…/releases/latest/download/install.{sh,ps1}` as well — both URLs work; the `raw.githubusercontent.com` form simply also works *before* the first Release.

## ⚡ Usage

## CLI Reference

GitNexus has a **two-tier CLI** designed for LLM agents:

- **9 agent commands** at the top level (query / refactor / verify):
  inspect, search, impact, rename, cypher, coverage, routes, scan, contracts
- **7 admin commands** under `gnx admin` (registry / hooks / destructive ops):
  install-hook, drop, prune, rename-branch, config, group, index

Run `gnx --help` for the agent surface. Run `gnx admin --help` for admin
operations. See `docs/specs/2026-05-15-gnx-cli-redesign-design.md`
for the full design.

### Quick start

```bash
# 1. Build a code graph for the current repo (Extremely fast, < 1s)
gnx admin index --repo .

# 2. Locate a symbol — exact-name lookup by default, `--mode bm25` for ranked BM25 search
gnx find "loginUser"

# 3. Extract all API Routes across the Microservice
gnx routes --repo .

# 4. Find a symbol's blast-radius / execution flow
gnx impact validateUser --direction upstream

# 5. Explore Context (Metadata, Decorators, Signatures)
gnx inspect validateUser
```

Every read-side command accepts `--format text|json|toon`. The default is the token-cheapest representation per command (most: `toon`; `find`: `text`; `cypher`/`coverage`: `json`; `coverage`: `md`/`compact`).

### Task → command

| Goal | Use |
|---|---|
| Index a fresh repo | `gnx admin index --repo .` (first query also auto-indexes) |
| Re-index after edits | Same — `admin index` is incremental (SHA-256 content hash per file) |
| Symbol exists? Where? | `gnx find <name>` (exact match) or `gnx find <fragment> --mode bm25` (ranked) |
| One symbol → metadata, callers, callees | `gnx inspect <name>` |
| If I edit X, what breaks? | `gnx impact <name> --direction upstream` |
| What does X depend on? | `gnx impact <name> --direction downstream` |
| Arbitrary graph traversal / source body | `gnx cypher 'MATCH (m:Method) WHERE … RETURN m.content'` |
| List every HTTP route | `gnx routes` |
| Who calls `POST /api/users`? | `gnx routes /api/users --method POST` |
| Where do we call external HTTP / DB / Redis / queue? | `gnx coverage --detailed` |
| Trace one execution flow start-to-finish | `gnx cypher` (use Cypher query language) |
| Architecture / hottest files / top symbols | `gnx coverage` |
| Coverage report (frameworks parsed, blind spots) | `gnx coverage` |
| What changed in this commit and what it ripples to | `gnx impact --since HEAD~1` |
| Rename a symbol across files (14 languages — see matrix `Rename` column) | `gnx rename --symbol old --new-name new --dry-run` then drop `--dry-run` |
| List repos this machine has indexed | `gnx coverage` (registry overview without `--repo`) |
| Re-register a `.gitnexus-rs/` folder after moving the repo | `gnx admin index --repo <path>` |
| Drop an index entirely | `gnx admin drop --repo <path>` |
| Multi-branch / multi-worktree workflows | `gnx admin install-hook`, `gnx admin prune --branch X`, `gnx admin rename-branch --from A --to B` |
| Interactive setup wizard | `gnx admin config` |
| Check if the on-disk graph is stale | `gnx coverage` (freshness in output) |
| List members of a graph community/cluster | `gnx cypher` (use Cypher query language) |

### MCP server (for LLM hosts)

`gnx` ships an MCP server exposing the 8 core commands as MCP tools.
Hosts that speak MCP (Claude Code, Cursor, Windsurf, Cline, Codex CLI,
Gemini CLI, etc.) can register `gnx` and call the tools autonomously.

```bash
# Inspect what tools will be exposed
gnx mcp tools

# Run the MCP server (default: spawn mode — fresh subprocess per call)
gnx mcp serve

# Or daemon mode — Engine mmap'd, mtime-remap on graph rebuild
# (full wiring lands with `gnx admin` TUI; for now spawn mode only)
gnx mcp serve --daemon
```

Manual host config example for Claude Code (`~/.config/claude-code/mcp-servers.json`):

```json
{
  "mcpServers": {
    "gnx": { "command": "gnx", "args": ["mcp", "serve"] }
  }
}
```

A `gnx admin` TUI for one-command installation across multiple hosts
ships in a follow-up release.

### Command reference

All commands resolve `.gitnexus-rs/graph.bin` from the current dir unless `--graph <path>` is given. Read-only commands take `--repo <name-or-path>` to disambiguate when multiple repos are registered.

#### Agent commands (top-level)

| Command | Purpose | Key flags |
|---|---|---|
| `inspect <name>` | One symbol → metadata, decorators, signature, callers, callees. | `--kind` · `--file_path` · `--relation_types` · `--include_tests` |
| `search <pattern>` | BM25 lexical symbol search by name. Output is partitioned into five independent top-20 buckets: `source` (production code), `tests`, `reference` (vendored/deps), `document`, `config`. Each hit includes a `language` field. | `--mode bm25` (no-op alias) · `--format` · `--batch` |
| `impact <name> --direction <dir>` | Blast radius / dependency traversal. `dir` ∈ `upstream` (who calls X), `downstream` (what X calls). | `--depth <n>` (default 5) · `--high-trust-only` (default true) · `--min-confidence <f>` · `--include-tests` · `--kind` · `--file_path` · `--since <ref>` |
| `rename --symbol <old> --new-name <new>` | AST-powered multi-file rename across 14 languages (Python, TS/TSX, JS, Rust, Java, Kotlin, C#, Go, PHP, Ruby, Swift, C, C++, Dart). Always run `--dry-run` first. | `--dry-run` · `--markdown` |
| `cypher '<query>'` | Arbitrary openCypher pattern matching. `m.content` returns source body. | `--format` |
| `coverage` | Registry overview, framework coverage, blind-spot catalog, graph freshness, and top symbols. Without `--repo`: lists all indexed repos. | `--detailed` · `--format compact\|json` |
| `routes [<path>]` | Enumerate every HTTP route extracted (declarative `@Get` and imperative `app.get()`). With `<path>`: route → handler → upstream callers. | `--method GET\|POST\|…` · `--depth <n>` (default 3) |
| `scan` | Calls to known HTTP / DB / Redis / queue clients; change detection. | `--since <ref>` · `--category http,db,redis,queue` |
| `contracts` | Verify API contracts. | — |

#### Admin commands (`gnx admin`)

| Command | Purpose | Key flags |
|---|---|---|
| `admin index --repo <path>` | Build / refresh the graph for `<path>`. Incremental by default (content-hash cache). | `--force` (full rebuild) · `--dump-resolver <file>` · `--no-cache` |
| `admin install-hook` | Install the git reference-transaction hook so branch switches auto-track. | `--force` · `--no-chain` |
| `admin drop [--repo <p>] [--all]` | Delete the `.gitnexus-rs/` for a repo (or all) and its registry entry. | — |
| `admin prune --branch <name> --repo <p>` | Drop a stale branch-scoped index dir. | — |
| `admin rename-branch --from <a> --to <b> --repo <p>` | Rename a branch's on-disk index. | — |
| `admin config` | Interactive TOML wizard for `.gitnexus-rs/config.toml`. | `--repo <p>` |
| `admin group` | Cross-repo group management. | — |

> Every command's flags can be re-confirmed with `gnx <command> --help`. The CLI is non-interactive by design (LLM-friendly): all flags surface via `--help`, all output goes to stdout in a parseable format.

## Language Matrix

graph-nexus's own per-language capability inventory across 31 supported languages. Each cell answers a single question: **for this language, do we extract this dimension yet?**

This matrix is *not* a parity scorecard against any other tool. We took design inspiration from GitNexus's 9-dimension breakdown (credit in the section above), but every cell describes the state of *our* implementation, scored against our roadmap — not against an external claim.

**Legend**:
- ✓ &nbsp;**implemented** — we extract this for this language today
- ☐ &nbsp;**feasible, not implemented yet** — the language has this concept; we could add it. Treat as a roadmap marker.
- — &nbsp;**not applicable** — the language doesn't have this concept (e.g. Dockerfile has no `Frameworks`).

> `—` is used wherever the language doesn't have the concept — predominantly below the divider (markup/config formats without that concept, e.g. Dockerfile has no `Frameworks`, YAML has no renameable identifiers), but also a handful of cells above the divider where the language genuinely lacks the concept (Go/Rust Ctor, JavaScript/Ruby Entry; see the per-cell notes below the table for the rationale). The matrix is fully resolved — no `☐` (feasible-but-not-implemented) cells remain anywhere.

| Language | Imports | Named | Exports | Heritage | Types | Ctor | Config | Frameworks | Entry | Call | Rename | Group extractor |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| TypeScript | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ (HTTP + gRPC) |
| JavaScript | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ (HTTP + gRPC) |
| Python | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ (HTTP + gRPC) |
| Java | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ (HTTP + gRPC) |
| Kotlin | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | —[^ge] |
| C# | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | —[^ge] |
| Go | ✓ | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ (HTTP + gRPC) |
| Rust | ✓ | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ (HTTP + gRPC) |
| PHP | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | —[^ge] |
| Ruby | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | — | ✓ | ✓ | —[^ge] |
| Swift | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | —[^ge] |
| C | ✓ | ✓ | ✓ | — | ✓ | — | ✓ | — | ✓ | ✓ | ✓ | —[^ge] |
| C++ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | —[^ge] |
| Dart | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | —[^ge] |
| ─── *structural-only rows below* ─── | | | | | | | | | | | | |
| Bash | ✓ | ✓ | n/a | n/a | n/a | n/a | n/a | — | — | ✓ | ✓ | — |
| Lua | ✓ | ✓ | ✓ | ✓ | n/a | — | n/a | — | — | ✓ | ✓ | — |
| Solidity | ✓ | ✓ | ✓ | ✓ | — | — | n/a | — | — | ✓ | ✓ | — |
| Crystal | ✓ | ✓ | ✓ | ✓ | — | — | n/a | — | — | ✓ | ✓ | — |
| Nim | ✓ | ✓ | ✓ | ✓ | — | — | n/a | — | — | ✓ | ✓ | — |
| Cairo | ✓ | ✓ | ✓ | — | — | — | n/a | — | — | ✓ | ✓ | — |
| Move | ✓ | ✓ | ✓ | n/a | — | n/a | n/a | — | — | ✓ | ✓ | — |
| Zig | ✓ | ✓ | ✓ | n/a | — | — | n/a | — | — | ✓ | ✓ | — |
| HCL | ✓ | ✓ | ✓ | n/a | — | n/a | ✓ | — | — | ✓ | ✓ | — |
| SQL | n/a | ✓ | n/a | ✓ | — | n/a | n/a | n/a | n/a | ✓ | ✓ | — |
| Verilog | ✓ | ✓ | ✓ | — | — | — | n/a | — | — | ✓ | ✓ | — |
| Vyper | ✓ | ✓ | ✓ | n/a | — | — | n/a | — | — | ✓ | ✓ | — |
| Markdown | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | — |
| GitHub Actions | ✓ | n/a | ✓ | n/a | n/a | n/a | ✓ | n/a | — | n/a | n/a | — |
| Docker Compose | — | n/a | n/a | n/a | n/a | n/a | ✓ | n/a | n/a | n/a | n/a | — |
| Dockerfile | ✓ | n/a | n/a | n/a | n/a | n/a | ✓ | n/a | — | n/a | n/a | — |
| YAML | n/a | n/a | n/a | n/a | n/a | n/a | ✓ | n/a | n/a | n/a | n/a | — |

[^ge]: Extractor stub only — first-wave group extractor coverage limited to Go / Python / JS / TS / Java / Rust.

**Per-cell notes** (where the cell shape needs context):
Bash Imports `source`/`.`; Lua Imports `require` + binding alias; Lua Heritage = `setmetatable(...,{__index=Parent})` heuristic; Ruby Named = `alias` keyword + `alias_method` + constant assignment (`MyConst = Other::Constant`) + `def_delegator`/`def_delegators`/`delegate` (with Forwardable mixin detection; cross-file `include Foo` propagation resolved via resolver Tier 2.75 HeritageScoped); Solidity Heritage = `is X, Y, Z`; SQL Heritage = FK `REFERENCES` clauses (inline, table-level, and named-constraint forms); GitHub Actions Imports = `uses:` directives (public tag/SHA refs, local composites, reusable workflows, cross-repo workflows); Dockerfile Imports = `FROM <base>`; C Named = `typedef` + `#define` / `preproc_function_def` + `extern` declarations (include-guard macros filtered; classified as Alias/Constant/Macro/Flag); Swift Named = `typealias` declarations + `@objc(extName)` rename attributes. Rename `n/a` on the 5 markup/config rows (Markdown, GitHub Actions, Docker Compose, Dockerfile, YAML) reflects that these formats carry keys/literal strings rather than re-bindable code identifiers — `gnx rename` would have nothing to rewrite. Ctor `—` on Go and Rust reflects that neither language has a language-level constructor — Go uses factory functions (`NewFoo()`) and Rust uses associated functions (`Foo::new()`) as idiomatic substitutes, but the cross-language Ctor extractor only emits `NodeKind::Constructor` for languages with a reserved ctor name (`__init__`, `initialize`, `__construct`, `constructor`, `Class::Class`). Entry `—` on JavaScript and Ruby reflects the absence of a language-level `main` convention (per `entry_points.rs` coverage table) — entry points still surface for these languages via route handlers and framework decorators, just not via a `main()` symbol. **Cell legend**: `✓` implemented · `—` concept exists in the language but not yet implemented · `n/a` language linguistically lacks this concept (e.g., Bash has no class system, so Heritage/Ctor/Types are n/a). Exports: Lua `function foo()` (top-level non-`local`); Crystal default-public minus `private`/`protected` modifier; Nim trailing `*` marker; Cairo / Zig / Move `pub`/`public`/`entry` keyword; HCL `output` block; Vyper `@external`/`@view`/`@payable` decorators; Verilog SystemVerilog `class_property` minus `local`/`protected` qualifier; GitHub Actions `jobs.*.outputs` + `on.workflow_call.outputs`. Named: Bash `alias` command; Lua `local M = require(...)` and dotted-path bindings (plain literal RHS filtered); Cairo `use X as Y` + `type X = Y`; Move `use ... as` alias clause (module + braced-member forms); Zig `const X = @import(...)` / `const X = Identifier` (numeric/string/bool literal RHS filtered via parser-side priority promotion); Crystal `alias X = Y`; Nim `type X = Y` with object/distinct/ref-type/tuple-object shapes filtered out (those stay Class); Vyper `from X import Y as Z` / `import X as Y` (source-line scan — grammar can't AST-parse the `as` clause); Solidity `using L for T` directives + `type C is uint256` user-defined value types; HCL `locals { }` block attributes (`output` blocks remain Const); SQL top-level `CREATE VIEW v AS ...` (column aliases `SELECT x AS y` not captured); Verilog SystemVerilog `typedef` declarations. Named `n/a` on GitHub Actions / Docker Compose / Dockerfile reflects that these YAML/Dockerfile formats use keyed top-level entries (services, jobs, `ARG`/`LABEL`) — those are configuration keys already captured by the Config column, not re-bindable alias declarations.

**Roadmap** — the matrix is now fully resolved to `✓` / `—` / `n/a`. No `☐` (feasible-but-not-implemented) cells remain — every `—` is a concrete gap, every `n/a` is a non-target.

**Recently shipped** (history, for context):
- Cross-language Constructor Inference (14 langs) with Python's `4e4fb1b` receiver-type binding as the reference prototype.
- Java static-import named bindings; C# `csproj` / `global.json` config; Exports for Go/Ruby/C/Dart per language conventions; cross-language Entry Point scorer combining routes + `main()` + framework decorators.
- Wave 2 (PR [#2](https://github.com/coseto6125/graph-nexus/pull/2)): Types for Go/C/C++ (declared types on params/returns/fields/vars); Config for PHP (`composer.json`) + Swift (`Package.swift`); Frameworks for JS (Express + Hapi) / Kotlin (Ktor) / Go (gin + echo) / PHP (Laravel).
- Wave 3 (this commit) ports the remaining 8 `☐` cells in the main table from upstream `_source_code/gitnexus`:
  - **Frameworks** via `astFrameworkPatterns` substring scans (`languages/{csharp,ruby,swift,c-cpp,dart}.ts`): C# (aspnet / signalr / blazor / efcore), Ruby (rails / sinatra), Swift (uikit / swiftui / vapor), C++ (qt), Dart (flutter / riverpod). The shared `framework_helpers::detect_ast_framework_patterns` walks each language's `FrameworkPatternSpec` table and emits one `RawFrameworkRef` per detected framework at module level. C Frameworks lands as `—` because upstream's `cProvider` defines no `astFrameworkPatterns` (qt is C++-only on `cppProvider`).
  - **Types** for Swift / Dart — declared types on parameters, properties, and top-level vars. Swift uses postfix `name: Type` syntax and reads the `type_annotation` node text directly; Dart uses prefix `Type name` with an unfielded `(type ...)` sibling captured positionally.
- Matrix-opt batch (HEAD `86e65a7`): Go per-struct-field visibility, Dart per-symbol underscore convention, Ruby `attr_*` metaprogramming + mixin tracking, TS/JS re-export alias preservation; in the extras section, Bash `source`/`.` imports, Lua `require` aliases + metatable inheritance + table-assigned methods, Solidity state-variable visibility. See `docs/specs/2026-05-15-matrix-optimization-opportunities.md`.
- Wave 4 (this commit) closes the last `☐` cells under the divider:
  - **Rename** for 12 code-extras (Bash, Lua, Solidity, Crystal, Nim, Cairo, Move, Zig, HCL, SQL, Verilog, Vyper) via per-language `identifier_finder/<lang>.rs` modules — each is a thin wrapper around the shared `find_by_kinds` walker keyed on the language's identifier node kinds. The five markup/config rows (Markdown, GitHub Actions, Docker Compose, Dockerfile, YAML) are now `—` because they carry keys / literal strings, not re-bindable code identifiers.
  - **SQL Heritage** via FK `REFERENCES` clauses — inline column-level, table-level, and named-constraint forms all push the referenced table name into the source table's `heritage`.
  - **GitHub Actions Imports** via `uses:` directives — public tag/SHA refs, local composites, reusable workflow files, and cross-repo reusable workflows all emit `RawImport` entries (also fixed 3 pre-existing parser bugs that were dropping imports silently).

### Call detection design

Call detection is centralised in `crates/graph-nexus-analyzer/src/calls.rs`. The hot helper is `extract_calls(root, source, nodes, call_kinds)`:

- Each language parser passes the tree-sitter node kinds that represent a call in its grammar — e.g., `["call_expression"]` for JS/TS, `["function_call"]` for Lua, `["call"]` for Python.
- The walker is grammar-agnostic: descends the AST once, collects every call site, extracts the callee text via `callee_name_from(node, source)`, and attaches each call to its enclosing `Function` / `Method` via `attach_to_enclosing(line, callee, nodes)` (smallest-span containment).
- OO languages additionally bind a **receiver type** (`obj.method` → know what `obj` is). Each lang has its own receiver-type module (`<lang>/receiver_types.rs`) tracking local variable annotations and class-scope `this`/`self`. The receiver type is stored on the RawCall so downstream resolution can pick the correct overload when method names collide.
- Reflection / dynamic dispatch (`getattr(self, name)()`, JS dynamic `obj[k]()`, etc.) is **not** speculatively resolved; it lands as a `BlindSpot` record (per the project's "honest unknown beats fabricated edge" principle).
- Call edges (`RelType::Calls`) are the largest single edge type in the graph; the saturating-conversion helper `safe_row` in calls.rs guards against rows exceeding `u32::MAX` corrupting call-to-function attribution.

## 🏗️ Architecture

```
crates/
├── graph-nexus-core        # Zero-copy graph (rkyv), Incremental Caching, Graph Queries
├── graph-nexus-analyzer    # Tree-sitter parsers, HTTP Route Detector, Framework Confidence
└── graph-nexus-cli         # `gnx` binary, Tantivy BM25 Engine, Token-optimized Output
```

The analyzer streams parsed nodes through an MPSC channel into a single builder thread that assembles the graph, applies Route & Document extraction rules, and writes a zero-copy `.gitnexus-rs/graph.bin`. Read operations (like `context` and `query`) memory-map this file directly for zero-latency lookups.

## ⚙️ Tuning

| Env var | Default | Effect |
|---|---|---|
| `GNX_MAX_FILE_BYTES` | `16777216` (16 MiB) | Skip source files larger than this during ingest. Caps worst-case worker RAM at `num_threads × MAX`. Raise for legitimate generated/compiled-output indexing; lower on memory-constrained machines. |
| `GNX_CSPROJ_MAX_DEPTH` | `4` | Directory recursion depth for `*.csproj` discovery. Raise for deeply-nested .NET monorepos. |

## Concurrency invariants

The audit at `docs/superpowers/specs/2026-05-16-concurrency-audit-design.md`
froze the following invariants. Any change to the parallel emit surface
(rayon pass2, Registry concurrent writes, StringPool intern, hook flock)
MUST keep these tests passing before merge.

1. **pass2 emit determinism** — `pass2_parallel_serial_identical_per_reltype`
   (`crates/graph-nexus-analyzer/src/resolution/builder.rs`) asserts identical
   `(source, target, RelType, reason)` set across serial dump path and
   parallel production path. Per-RelType stratification means a regression
   points at the rel-type that diverged.
2. **GraphBuilder order independence** — `graph_builder_order_independence_under_default_threads`
   (`crates/graph-nexus-analyzer/tests/concurrency_graph_builder_order.rs`)
   asserts canonical projection (sorted Nodes/Edges/Files → BLAKE3) is
   identical across ingest permutations and across repeated builds.
3. **Registry inter-process flock** — `registry_concurrent_writers_converge`
   (`crates/graph-nexus-core/tests/concurrency_registry_writers.rs`)
   asserts N concurrent child-process upserts all converge into the final
   registry. Models real Claude Code hook contention.
4. **StringPool intern dedup** — `string_pool_mutex_wrapped_concurrent_dedupe`
   (`crates/graph-nexus-core/tests/concurrency_string_pool_intern.rs`)
   asserts that when `StringPool` is shared across threads it MUST be
   `Mutex`/`RwLock` wrapped (the type system enforces this; the test pins
   that the wrap preserves dedup).
5. **Hook flock serialisation** — `hook_concurrent_spawn_flock_serializes`
   (`crates/graph-nexus-cli/tests/concurrency_hook_flock.rs`) asserts two
   concurrent hook spawns produce exactly one reindex side-effect; the
   second no-ops cleanly with exit 0.

Run `./scripts/audit-concurrency.sh` to re-verify all five.

## Onboarding skill for AI agents

`docs/skills/gnx-onboard/` ships a layered SKILL pack that turns any LLM
agent into a personalized installation + configuration wizard for `gnx`.

Four ways to use it:

### (a) URL bootstrap — any LLM agent

Paste into your agent chat:

```
Fetch https://raw.githubusercontent.com/<owner>/gitnexus-rs/main/docs/skills/gnx-onboard/SKILL.md
and follow it as my onboarding wizard for graph-nexus.
```

The agent reads `SKILL.md`, probes your system, and walks you through
install + first-index + group + MCP setup, tailored to the prompts /
preferences already in its context.

### (b) ShareOnboardingGuide — Claude Code (lowest friction)

From this repo's checkout:

```bash
cd docs/skills/gnx-onboard
# In Claude Code, invoke the ShareOnboardingGuide tool
# It uploads ONBOARDING.md and returns a short link
```

Send that link to a teammate. They open it in their Claude Code session
and the wizard auto-loads.

### (c) Plugin install — Claude Code (advanced)

```bash
# Pull only the SKILL pack — avoids downloading the whole Rust workspace
git clone --depth=1 --filter=blob:none --sparse \
    https://github.com/<owner>/gitnexus-rs ~/.claude/plugins/gnx-onboard-src
cd ~/.claude/plugins/gnx-onboard-src
git sparse-checkout set docs/skills/gnx-onboard
ln -s docs/skills/gnx-onboard ~/.claude/skills/gnx-onboard
```

### (d) Manual git clone — any agent

Same `--depth=1 --filter=blob:none --sparse` + `git sparse-checkout set
docs/skills/gnx-onboard` recipe, dropped into your agent's skill / rule
directory.

### How the SKILL is structured (for SKILL authors)

- `SKILL.md` — Layer 1 entry, frontmatter + jump table + directives.
- `guides/01-…05-…md` — Layer 2 phase guides.
- `_shared/cli/<version>/<cmd>.md` — Layer 3 auto-generated CLI reference (one set per gnx version).
- `_shared/refs/{persona-inference,env-detect,recommendation-templates}.md` — Layer 3 hand-written rule tables.
- `ONBOARDING.md` — build artifact (aggregator output). Do not hand-edit.

Tooling lives at `tools/{lint-skill,aggregate,gen-cli-ref,test-persona-rules}.sh`.
CI in `.github/workflows/skill-{aggregate,cli-ref}.yml`.
Spec: `docs/superpowers/specs/2026-05-18-gnx-onboard-skill-design.md`.

## 📄 License

Licensed under [PolyForm Noncommercial 1.0.0](./LICENSE). Personal use, research,
hobby projects, and noncommercial organizations are explicitly permitted purposes.

**Commercial use is not granted by this license.** If you need commercial rights,
contact the upstream GitNexus author Abhigyan Patwari.

## 🙏 Acknowledgments

*   [GitNexus](https://github.com/abhigyanpatwari/GitNexus) by Abhigyan Patwari — original design, CLI surface, and conceptual model.
*   [tree-sitter](https://tree-sitter.github.io/) — robust incremental AST parsing.
*   [rkyv](https://rkyv.org/) — ultimate zero-copy deserialization.
*   [Tantivy](https://github.com/quickwit-oss/tantivy) — blazing fast Rust full-text search.
