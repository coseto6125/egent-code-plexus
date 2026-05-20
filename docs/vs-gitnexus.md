# vs. upstream GitNexus

> **Code Graph Nexus is not a drop-in replacement for [GitNexus](https://github.com/abhigyanpatwari/GitNexus).** Same conceptual model — a structural knowledge graph of a repo — different audience, different runtime, different output discipline. This document explains where the two projects diverge and why.

`cgn` exists because we wanted to use GitNexus's idea inside autonomous LLM coding workflows, and the original product was built for a different shape of consumer: humans in IDEs and long-running agent platforms with MCP wiring. The differences below are not "Rust is faster" — they are decisions about who the consumer is and what an honest answer to that consumer looks like.

---

## 1. Audience

| | GitNexus | Code Graph Nexus |
|---|---|---|
| Primary consumer | Human developers + IDE integration | Autonomous AI code agents |
| Interaction loop | Developer reads UI, navigates, refines query | Agent emits a shell call, parses stdout, decides next action |
| Token budget per query | Effectively unbounded (humans skim) | Strict — each token eats the agent's context window |

The audience choice cascades into every other row of this comparison. GitNexus optimises for *legibility and ergonomics*; `cgn` optimises for *predictable cheap calls that an agent can fire dozens of times per task without warm-up cost*.

## 2. Runtime model

**GitNexus** runs as a long-lived MCP server. The graph is held in memory, the server stays warm, MCP tools route through host integrations (Claude Code, Cursor, Windsurf, Cline, Codex). The benefit: rich tool ergonomics, resources, prompts, hooks, generated skills — a full agent platform.

**`cgn`** is a stateless one-shot CLI. Every invocation is a fresh process that `mmap`s a zero-copy `rkyv` graph file off disk and answers one query. There is no server, no daemon by default, no state to manage between calls.

The tradeoff is real and goes both directions:

- *GitNexus wins* on rich session-aware features (cross-call memoisation, server-held caches, generated repo skills, MCP resources).
- *`cgn` wins* on cost-per-call predictability. A `cypher` query takes 9 ms cold including process startup. An agent can fire 30+ queries per task without amortising a warm-up phase. There is no "server died, please restart" failure mode, no port allocation, no state synchronisation.

`cgn` ships an MCP server too (`cgn admin mcp serve`), but it is a thin shim over the same one-shot binary, not a fundamentally different runtime.

## 3. Honesty about unresolved edges

This is the most important philosophical difference.

When the analyser sees `import { X } from 'unresolved-module'` or `getattr(obj, name)()` or a dynamic dispatch site whose target can't be statically determined, the two projects make opposite choices:

**GitNexus** emits a heuristic-guessed edge (e.g. Jaccard string similarity over candidate names) so the graph stays connected and readable. This is the right call for human consumers — a graph with too many missing edges looks "broken" to a developer skimming a UI.

**`cgn`** emits a `BlindSpot` record, not an edge. The graph admits "I cannot tell what this call resolves to" instead of inventing a likely-wrong answer.

The reason is consumer-specific: an LLM agent acts on what the graph says. If `cgn` returns a guessed edge with 60% confidence, the agent treats it as a fact and may rewrite, refactor, or generate tests against that wrong target. A hallucinated dependency that an agent then commits is much more expensive than an honest `BlindSpot` the agent can route around with a clarifying question.

> **Rule of thumb:** if the graph is read by humans who can ignore obvious nonsense, prefer GitNexus's "guess and keep connected." If the graph is read by an agent that will execute downstream actions, prefer `cgn`'s "I don't know" record.

## 4. Output format

| | GitNexus | Code Graph Nexus |
|---|---|---|
| Default format | Wiki / UI rendering | [TOON](https://crates.io/crates/etoon) (token-optimised) |
| Optional formats | MCP-rich responses, generated skills | `--format text\|json\|toon` per command |
| Format goal | Human readability | Bytes-per-fact ratio |

A symbol-context dump in MCP-rich form might be 800 tokens. The same dump in TOON is closer to 200, with no information loss for an agent that knows the schema. Multiplied across the 20–50 queries an agent fires per task, this saves multi-kilobyte chunks of context window per task — which an agent will spend on thinking instead of layout.

`cgn` picks the cheapest format per command as the default: most read commands default to `toon`; `find` defaults to plain `text` (already minimal); `cypher` and `coverage` default to `json` (structurally rich, expected to be parsed). Override anywhere with `--format`.

## 5. Language breadth vs depth

GitNexus parses **14 languages**: TypeScript, JavaScript, Python, Java, Kotlin, C#, Go, Rust, PHP, Ruby, Swift, C, C++, Dart. Each gets the full 9-dimension treatment: imports, named bindings, exports, heritage, types, constructor inference, config, frameworks, entry points.

`cgn` parses **31**: the same 14 plus Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig.

**This is breadth, not parity.** The 17 extension languages get structural coverage (functions / classes / methods / imports / calls) but not the full 9-dimension matrix — see [language-matrix.md](./language-matrix.md) for the per-language scorecard.

The motivation: modern repos are polyglot. A typical SaaS backend has Python service code, TypeScript frontend, SQL migrations, Dockerfiles, GitHub Actions workflows, Terraform (HCL) infrastructure. A Web3 project mixes Solidity, Move, Vyper, Cairo. A systems project mixes Rust, C, Zig, Bash. GitNexus answers structural questions only inside the 14-language code zone; everything else is a black hole. `cgn` extends the graph into the surrounding DevOps / infra / config / contract zone — still structural, but enough to answer "where does CI invoke this script" or "which contract calls this function" without leaving the tool.

The honest tradeoff: GitNexus knows more about *any one* of its 14 languages than `cgn` knows about, say, Bash or YAML. If your repo is monolingual and you need deep semantic analysis (full Heritage chains, Constructor Inference, Framework-specific bindings), GitNexus is the better fit. If your repo is polyglot and you mostly need "what calls what, across whatever languages", `cgn` is the better fit.

## 6. Tool & integration surface

| LLM-facing area | GitNexus | Code Graph Nexus |
|---|---|---|
| Agent integration | MCP server, resources, prompts, setup, hooks, generated skills | Stateless CLI + optional MCP server (`cgn admin mcp serve`) |
| Core query commands | `query`, `context`, `impact`, `detect_changes`, `rename`, `cypher`, group tools | `inspect`, `find`, `impact`, `routes`, `cypher`, `coverage`, `rename`, `contracts`, `diff` |
| Workflow tools | rich MCP resources, generated repo skills | `tool-map` (egress calls), `shape-check` (response-shape drift), `review` (audit aggregator), `peers` (multi-session collab) |
| Search | BM25 + semantic + RRF hybrid (documented) | Tantivy BM25 + per-name substring fallback |
| Storage | Node.js + LadybugDB | Rust + `rkyv` zero-copy mmap |

The command vocabularies have *similar shapes* but the verbs differ in scope. GitNexus's `context` is similar to `cgn inspect`. GitNexus's `detect_changes` overlaps `cgn diff` + `cgn impact --since`. `cgn` adds workflow-specific verbs that don't have GitNexus equivalents — `tool-map` (where does my code touch external HTTP/DB/Redis/queue), `shape-check` (does the HTTP consumer's expected response shape match the producer's actual shape), `review` (one-shot audit over the change-set). These exist because they are common LLM-agent sub-tasks worth a dedicated verb.

## 7. Storage layer

GitNexus uses LadybugDB — a Node.js graph database — which gives it indexed property graph queries, transactions, and durable storage.

`cgn` stores the graph as a single `rkyv`-serialised binary file (`.cgn/graph.bin`) and `mmap`s it on every command. The on-disk layout is fixed at index time. Queries that need indexes (BM25 lexical search) maintain a separate Tantivy index alongside the graph file.

The tradeoffs:

- `cgn` cannot do mid-graph mutations — every rebuild is a full or incremental re-serialisation. It compensates with an aggressive xxh3_64 content cache so incremental rebuilds on a 22k-file repo take < 0.25 s.
- `cgn` has no concurrency control beyond an OS-level flock. Two `cgn admin index` processes will serialise.
- `cgn` reads are zero-copy. Opening a graph and answering a `cypher` query takes 9 ms cold, dominated by process startup, not graph access. This is the property that makes the one-shot-CLI model viable.

LadybugDB is the right pick if you want a persistent, mutable, multi-writer graph backend with rich query planning. `rkyv + mmap` is the right pick if you want zero-latency reads from a stateless CLI and can accept "regenerate the file" as the write model.

## 8. When to choose which

**Choose GitNexus if:**
- You're integrating into a Node-based agent runtime with strong MCP/editor support
- Your repo is one of the 14 supported languages and you need depth on every dimension
- You want the agent platform features (resources, generated skills, hooks)
- You're a human developer who wants to *navigate* the graph in a UI, not just programmatically query it

**Choose Code Graph Nexus if:**
- You're building LLM tooling that wants a small executable with few moving parts
- Your repo is polyglot (DevOps configs, Web3 contracts, infra-as-code, build glue) and you need at least *structural* visibility everywhere
- You value honest blind-spot records over heuristic-guessed edges (agent will act on the graph)
- You need sub-second per-query latency with no warm-up cost (agent fires many queries per task)
- You're shell-mediating an LLM and want token-cheap output (TOON / compact JSON)

The two are not mutually exclusive. Several teams run GitNexus as the human-facing MCP server and `cgn` as the agent-facing shell tool over the same repo — different consumers, different surfaces.

---

## Attribution

`cgn` is a derivative work of GitNexus. The original design, CLI surface, and conceptual model are the work of [Abhigyan Patwari](https://github.com/abhigyanpatwari). `cgn` is not affiliated with or endorsed by the upstream GitNexus project. See [NOTICES.md](../LICENSES/NOTICES.md) for the full third-party attribution list.
