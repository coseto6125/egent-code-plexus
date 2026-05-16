# CLAUDE.md

This project is an **LLM-first** code intelligence graph. The `gnx` CLI is the product; LLMs and AI code agents are the user. Read this before making changes.

## Priority order

1. **Per-query latency** — agents fire 30+ queries per task. Cold index <5 s / 25 k files; per-query target <30 ms for cypher / context / impact / route-map.
2. **LLM helpfulness** — answers must reduce hallucination risk for the consuming model.
3. **Signal density** — every byte of output costs context window. No prose, no UI cruft, no "I think maybe".

These are ordered. When perf and prettiness conflict, perf wins.

## Output discipline

- Default formats: `toon` for `inspect / coverage / contracts / routes`, `json` for `cypher`, `text` for `search / scan / impact / rename` (human-debugging path).
- Minimal but unambiguous — never strip a field that the LLM needs to disambiguate the answer (file path, line, kind, repo, score source).
- **Never fabricate.** Honest "no data" (empty array, `BlindSpot` record, `null` field) always beats a guess. Heuristic edges with `<0.7` confidence must be tagged, not promoted.
- No trailing summaries inside structured payloads. Summary belongs in a sibling `summary` field, not interleaved.

## Performance non-negotiables

- Hot paths: `pre_tool_use::handle`, `compute_hits`, `dispatch_by_mode`. Don't add allocations / file I/O / network without justification.
- Auto-ensure reindex must not block agent commands beyond the one-time cost. Background spawn via `flock -n` is the established pattern.
- Profile before claiming a perf win. Canonical bench: `python scripts/benchmark_gnx.py`.
- Use rayon for independent fan-outs; rkyv archived access (zero-copy) over owned materialization where feasible.

## Parser / core-feature changes require 14-language coverage

Any change to:
- `crates/graph-nexus-analyzer/src/<lang>/parser.rs` shared logic
- Tree-sitter query templates
- Graph-construction primitives (`Node` / `Edge` / `RelType` / resolution rules)
- Search / impact / context core algorithms

…must ship with tests covering **at least these 14 mainstream languages**: TypeScript, JavaScript, Python, Java, Kotlin, C#, Go, Rust, PHP, Ruby, Swift, C, C++, Dart.

Existing test pattern: `crates/graph-nexus-analyzer/tests/<lang>_<dimension>.rs` (e.g. `go_frameworks.rs`, `swift_type_annotations.rs`, `ruby_named.rs`). New dimensions get their own per-language files.

Single-language tests for a multi-language change get rejected — mixed-stack repos are the load-bearing use case (DevOps + Web3 + monorepos).

## Using `gnx` during development

`gnx` is the primary navigation tool for this codebase. When you use it:

- **Surface anomalies immediately.** If a query returns wrong file:line, missing symbol that grep finds, empty result where content exists, or unexpected auto-reindex behavior — stop and report it. Likely a real bug for the consuming LLMs, not a skill issue.
- **Capture commands verbatim** when reporting. `gnx <subcommand> --flag value` + stderr + actual vs expected output.
- **Don't suppress fallbacks** — `→ vector: ... — falling back to bm25` lines exist to be seen. If they fire under conditions where they shouldn't, that's the bug.

## Workspace

- `crates/graph-nexus-core` — `ZeroCopyGraph` (rkyv), `StringPool`, `Registry`, `BlindSpot`
- `crates/graph-nexus-analyzer` — per-language tree-sitter parsers, framework detectors, route detector, embeddings
- `crates/graph-nexus-cli` — `gnx` binary, hooks, engines, output formats (package name `graph-nexus`, lib `graph_nexus_cli`, bin `gnx`)
- `crates/graph-nexus-mcp` — MCP server (pre-1.0)

Commands:
- Build: `cargo build -p graph-nexus --bin gnx --release`
- Test: `cargo test -p graph-nexus --tests` (CLI) and `cargo test -p graph-nexus-analyzer` (parsers)
- Lint: `cargo clippy -p graph-nexus --tests` and `cargo clippy -p graph-nexus-analyzer`
- Format (touched files only — avoid `cargo fmt -p` blast radius): `rustfmt --edition 2021 <file>...`

## Style minimums

- No comments explaining WHAT — identifiers self-describe.
- WHY-only comments: hidden invariant, workaround, surprising behavior, non-obvious perf decision.
- Surgical changes — every line traces to the task. Flag unrelated issues, don't bundle drive-by fixes.
- Match existing style even if suboptimal; flag, don't silently "improve".
- Tests for new features ship in the same change. Bug fixes ship with a failing regression test first.
