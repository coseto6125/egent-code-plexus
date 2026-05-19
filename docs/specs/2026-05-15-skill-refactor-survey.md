# Skill Refactor Survey — cgn vs Upstream gitnexus

**Date:** 2026-05-15
**Purpose:** Inventory what we have, what upstream has, where the gaps are.
Foundation for refactoring upstream skills (`._source_code/gitnexus-claude-plugin/skills/`)
into cgn-native skills shipped via `.claude-plugin/`.

---

## 1. cgn CLI Inventory (Source of Truth)

Read from `crates/cgn-cli/src/commands/*.rs` Args structs.
Binary name: `code-graph-nexus` (typically aliased as `cgn`).
Global flag: `--graph <path>` (default `.cgn/graph.bin`).

### 1.1 Read-side commands

| Cmd | Required flags | Optional flags | Default output | Notes |
|---|---|---|---|---|
| `context` | `--name <symbol>` | `--repo`, `--format` | `toon` | 360° view of one symbol |
| `query` | `--query <str>` | `--repo`, `--format` | `text` | Substring search by name |
| `impact` | `--target <uid>`, `--direction up\|down` | `--depth=5`, `--repo`, `--format=toon`, `--high-trust-only` | `toon` | Blast radius traversal; `high-trust-only` drops <0.8 conf edges |
| `cypher` | `<query>` (positional) | `--repo`, `--format=json` | `json` | Minimal subset: `MATCH (a:Kind)-[r:Rel]->(b:Kind) [WHERE a.name='Val'] RETURN a,b` |
| `route_map` | — | `--repo`, `--format=toon` | `toon` | All HTTP routes |
| `detect_changes` | — | `--scope=unstaged`, `--base-ref`, `--repo`, `--format=toon`, `--kind`, `--include-tests`, `--high-trust-only` | `toon` | Git-diff impact. `--scope compare --base-ref HEAD~1` for pre-commit |
| `list` | — | `--format=toon` | `toon` | All indexed repos |
| `summarize` | — | `--repo`, `--top-files=10`, `--top-communities=10`, `--top-symbols=3`, `--format=md`, `--output`, `--include-orphans` | `md` | LLM project overview |
| `doctor` | — | `--format=compact\|json` | `compact` | **LLM contract**: framework coverage + blind-spot catalog + confidence thresholds |

### 1.2 Write/lifecycle commands

| Cmd | Required flags | Optional flags | Purpose |
|---|---|---|---|
| `analyze` | `--repo <path>` | `--embeddings`, `--dump-resolver <path>` | Build/refresh index |
| `init` | — | `--force`, `--no-chain` | Install git hook for branch tracking |
| `prune` | `--branch`, `--repo` | — | Remove orphan branch index |
| `rename-branch` | `--from`, `--to`, `--repo` | — | Rename a branch's index dir |
| `rename` | `--symbol`, `--new-name` | `--repo`, `--dry-run` | AST-powered multi-file symbol rename (**Python MVP**; multi-lang follow-up per language) |

### 1.3 Tooling/internal

| Cmd | Purpose |
|---|---|
| `verify-resolver` (`--oracle --cgn --lang --report`) | Diff resolver dump vs language oracle (TS/Py/Rs) |
| `hook-handle`, `hook-watcher` | Internal (hidden) — invoked by the git hook |

### 1.4 Output formats

- `toon` — TOON serialization (most read commands' default; compact, LLM-friendly)
- `json` — structured JSON
- `text` — human prose
- `md` — markdown (summarize default)
- `compact` — YAML-ish (doctor default)

### 1.5 LLM-impactful capabilities not surfaced by command name

These ride inside `analyze` / are visible via `doctor`:

- **Blind spots** — `eval` / `exec` / `compile` / `import_module` / `__import__` / cross-`getattr` sites tagged for LLM review (Python)
- **Receiver type binding** — `x: Apple` → `Apple.eat` resolved with high confidence (Python; merged today)
- **Path aliases** — `tsconfig.json` `compilerOptions.paths` expansion (TS; merged today, CLI side pending)
- **Fan-out resolution** — `getattr(self, name)()` emits N candidate edges with `base_conf / sqrt(N)` decay + `reason: "reflection-getattr-fanout"`
- **Framework gates** — `has_import_from` proves Django/FastAPI/Celery imported before claiming framework membership
- **Framework refs** — FastAPI `Depends()`, Django `@receiver` / `signal.connect`, Celery `@task`, Spring `@Autowired`, etc. → low-confidence `References` edges with reason tags

---

## 2. Upstream MCP Tools Inventory

Read from `._source_code/gitnexus-claude-plugin/skills/gitnexus-guide/SKILL.md`.
All accessed via MCP (`gitnexus_<tool>`), not a CLI.

| Tool | What it does |
|---|---|
| `query` | Process-grouped code intelligence — execution flows for a concept |
| `context` | 360° symbol view |
| `impact` | Blast radius at depth 1/2/3 with confidence |
| `detect_changes` | Git-diff impact |
| `rename` | Multi-file coordinated rename with confidence-tagged edits |
| `cypher` | Raw graph queries against a Cypher-like schema |
| `list_repos` | Discover indexed repos |

### Upstream graph schema (per `gitnexus-guide`)

- **Nodes:** File, Function, Class, Interface, Method, Community, Process
- **Edges (via `CodeRelation.type`):** CALLS, IMPORTS, EXTENDS, IMPLEMENTS, DEFINES, MEMBER_OF, STEP_IN_PROCESS

### Upstream resources (MCP resource URIs)

| Resource | Content |
|---|---|
| `gitnexus://repo/{name}/context` | Stats, staleness check |
| `gitnexus://repo/{name}/clusters` | All functional areas with cohesion scores |
| `gitnexus://repo/{name}/cluster/{name}` | Area members |
| `gitnexus://repo/{name}/processes` | All execution flows |
| `gitnexus://repo/{name}/process/{name}` | Step-by-step trace |
| `gitnexus://repo/{name}/schema` | Graph schema for Cypher |

---

## 3. Upstream Skills Inventory

7 skill files, ~700 lines total. Front-matter format: `name`, `description`.

| Skill | LOC | Anchor task |
|---|---|---|
| `gitnexus-guide` | 64 | Meta-router: which skill to read for which task |
| `gitnexus-cli` | 85 | `npx gitnexus analyze/status/clean/wiki/list` |
| `gitnexus-exploring` | 78 | "How does X work?" — query + context + processes |
| `gitnexus-impact-analysis` | 97 | "What breaks if I change X?" |
| `gitnexus-debugging` | 89 | "Why is X failing?" |
| `gitnexus-refactoring` | 121 | Rename/extract/split with rename tool |
| `gitnexus-pr-review` | 163 | Multi-step PR review with risk |

Common skeleton:
- Front-matter `name` + `description` (with example user prompts)
- `When to Use` bullets
- `Workflow` numbered block
- `Checklist` checkbox list
- `Tools` examples with output mockups
- `Example` walkthrough

---

## 4. Tool / Capability Delta (Upstream → cgn)

### 4.1 Direct map

| Upstream MCP | cgn CLI | Parity |
|---|---|---|
| `query` | `cgn search --query "..."` | ✅ same intent |
| `context` | `cgn inspect X` | ✅ |
| `impact` | `cgn impact X --direction upstream --depth N --high-trust-only` | ✅ + `high-trust-only` is **ours** |
| `detect_changes` | `cgn impact --since HEAD~1 --kind --include-tests --high-trust-only` | ✅ + scope variants are **ours** |
| `list_repos` | `cgn coverage` | ✅ |
| `rename` | `cgn rename --symbol X --new-name Y --repo P --dry-run` | ✅ **Python only (MVP, merged 2026-05-15)** — multi-lang remaining; see `commands/rename.rs` |
| `cypher` | `cgn cypher "<query>" --repo P --format json` | ✅ **minimal subset (merged)** — supports `MATCH (a:Kind)-[r:Rel]->(b:Kind) [WHERE a.name='Val'] RETURN a,b` |

### 4.2 Upstream MCP resources → cgn

| Upstream `gitnexus://...` resource | cgn alternative |
|---|---|
| `/context` (stats, staleness) | `cgn coverage` (md/json) — overlapping intent, richer output |
| `/clusters` | partly in `cgn coverage` (`--top-communities`) |
| `/cluster/{name}` | **MISSING** |
| `/processes` | partly in `cgn coverage` |
| `/process/{name}` | **MISSING** (no step-by-step trace exposure) |
| `/schema` | embedded in `cgn coverage` (relations + node kinds) |

### 4.3 cgn extras (no upstream peer)

| cgn | Why it matters for LLM |
|---|---|
| `cgn coverage` | Surfaces the **whole contract**: which frameworks are detected with what confidence, which patterns are blind spots, where to look. The single biggest hallucination-reducer we have. |
| `cgn coverage --detailed` | LLM project overview — meant to be the FIRST thing dropped into the LLM's context window |
| `cgn routes` | Stand-alone HTTP route inventory (upstream has it implicit in cypher) |
| Internal resolver validation | Tooling, not LLM-facing |
| Blind spots, receiver types, path aliases, fan-out, framework gates | (1.5) Surfaced via `doctor`; visible in graph as edges with `reason` tags + decayed confidence |

### 4.4 Graph schema delta

| Aspect | Upstream | cgn |
|---|---|---|
| Node kinds | File, Function, Class, Interface, Method, Community, Process | Method, Function, Class, Property, Const, Variable, Route, File, Process |
| Differences | Has `Interface`, `Community` as nodes | Has `Property`, `Const`, `Variable`, `Route` as nodes; `Community` is metadata, not a node |
| Edge types | CALLS, IMPORTS, EXTENDS, IMPLEMENTS, DEFINES, MEMBER_OF, STEP_IN_PROCESS | CALLS, IMPORTS, EXTENDS, HAS_METHOD, HANDLES_ROUTE, FETCHES, METHOD_OVERRIDES, ACCESSES, MEMBER_OF, CONTAINS, DEFINES, **References** (low-conf framework refs) |
| Differences | `IMPLEMENTS`, `STEP_IN_PROCESS` | `HAS_METHOD`, `HANDLES_ROUTE`, `FETCHES`, `METHOD_OVERRIDES`, `ACCESSES`, `CONTAINS`, **`References`** with `reason` tag + decayed confidence |

### 4.5 Confidence model delta

- **Upstream:** edges have confidence; `impact` accepts `minConfidence`.
- **cgn:** same confidence model PLUS `--high-trust-only` shortcut (≥0.8 cutoff) on `impact` + `detect_changes`. Plus the `reason` string on each edge (`framework-aware-fastapi-depends`, `reflection-getattr-fanout`, etc.) — actionable for LLM to know **why** the resolver picked that target.

---

## 5. Refactor Recommendations per Skill

| Upstream skill | Refactor verdict | Key edits |
|---|---|---|
| `gitnexus-guide` | **Rewrite** | New skill index table pointing at our 6 refactored skills. Replace MCP tool list with `cgn <cmd>` table. Drop resource URIs (we don't have MCP server). Update schema block (our node kinds + relations). Add a "blind spots awareness" callout. |
| `gitnexus-cli` | **Rewrite + expand** | `cgn admin index` (replace `npx gitnexus analyze`). Add `cgn coverage`, `cgn coverage --detailed`, `cgn admin install-hook`, `cgn admin prune`, `cgn admin rename-branch`. Drop `wiki` (we don't have it). |
| `gitnexus-exploring` | **Adapt** | Replace `gitnexus_*` calls with `cgn <cmd>`. Replace process-resource reads with `cgn coverage --detailed` snippet or `cgn impact --since HEAD~1`. Add a "Start with `cgn coverage`" preamble so LLM knows what's reliable. |
| `gitnexus-impact-analysis` | **Adapt** | Replace tool calls. Add `--high-trust-only` recommendation. Add receiver-type / blind-spot awareness ("if target is in a blind spot, `impact` may show **incomplete** upstream callers — surface that risk"). |
| `gitnexus-debugging` | **Adapt** | Replace tool calls. Add blind-spot section: "if the suspect symbol is reached only via eval/exec/dynamic-import, `cgn coverage` will list it under blind spots — `cgn impact` won't find those callers." |
| `gitnexus-refactoring` | **Adapt (Python-only)** | `cgn rename` **landed 2026-05-15 (Python MVP)** — see `commands/rename.rs`. Skill can be written against this for Python projects; other languages need `identifier_finder` peers (per-lang follow-up). |
| `gitnexus-pr-review` | **Adapt** | Replace tool calls. Add `cgn coverage` step before the review (so LLM knows framework coverage). Add receiver-type / blind-spot awareness in the risk model. Multi-step workflow stays — it's universal. |

### 5.1 New skill we should add (no upstream peer)

**`cgn-onboarding`** (or merge into `gitnexus-guide`):
> When first encountering an unfamiliar repo: 1) `cgn coverage` to see the contract, 2) `cgn coverage --detailed` to get the project map, 3) only then drop into `cgn search` / `cgn inspect`. Three steps, predictable token cost.

---

## 6. Open Decisions

1. **Skill name prefix**: keep `gitnexus-*` (back-compat with upstream skill discovery) or switch to `cgn-*` (match our CLI binary)?
2. **Refactoring scope**: 7-of-7 in one PR, or land 3 (`-cli`, `-exploring`, `-impact-analysis`) first and follow with 4?
3. **Rename support**: ✅ **Python MVP merged 2026-05-15** (`commands/rename.rs`). Remaining: extend `identifier_finder` to TS / Rust / Go / Java / JS / C# / Ruby / PHP / Kotlin / Swift / Dart / C / C++ (13 langs). The pattern is repeatable per-language — one commit each per the spec roadmap. |
4. **Location**: `.claude-plugin/skills/<name>/SKILL.md` (mirror upstream layout) — confirm.
5. **`plugin.json`**: write a new one with our `name`/`description`/`version`/`repo`?
6. **MCP server**: upstream's skills assume an MCP server is running. We have only CLI. Should our skills explicitly document "use the CLI, no MCP needed"?

---

## 7. Architecture Note

Per the global `~/.claude/CLAUDE.md` GitNexus Workflow section, the user has been describing
a richer wrapper command surface. As of 2026-05-15 the gap is narrower than originally noted:

| Mentioned in global CLAUDE.md | Status in cgn |
|---|---|
| `cgn cypher` | ✅ landed (minimal MATCH subset) |
| `cgn rename` | ✅ Python MVP landed; multi-lang remaining |
| `cgn routes /path` | ✅ landed (HTTP route inventory) |
| `cgn shape_check` | ❌ no subcommand |
| `cgn coverage --detailed` | ✅ landed (tool inventory) |
| `cgn coverage` | ✅ landed (repo coverage) |
| (removed old commands) | ✅ all consolidated into admin/impact/search/inspect/routes/coverage |

The refactored skills should document the four MISSING items as "consider implementing"
or "use workaround X", and treat the two LANDED items as documented surface.
