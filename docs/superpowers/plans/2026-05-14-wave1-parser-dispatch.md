# Wave 1 Parser Dispatch Plan

**Goal:** Add 7 new language providers to `gnx-analyzer` in parallel: Lua, Solidity, Bash, Zig, Move, Crystal, Dockerfile.

**Why these 7:** Per `2026-05-14-gnx-rs-parser-expansion.md` Wave 1 — the A-tier "high confidence" set where tree-sitter grammar is mature and language semantics map cleanly to gnx-rs's NodeKind/RelType vocabulary.

**Date:** 2026-05-14

---

## Phase 0 — Status (done before dispatch)

- ✅ Worker brief written: `docs/superpowers/plans/parser-worker-brief.md`
- ✅ Verified Swift `@name.X` convention is intentional legacy, not a bug — workers will use majority `@X.name` convention
- ✅ Verified `tests/parity/run_parity.py` exists
- ✅ Confirmed registration sites in `crates/gnx-cli/src/commands/analyze.rs` (lines ~50 and ~76)
- ✅ Confirmed `crates/gnx-analyzer/src/lib.rs` is a flat `pub mod <lang>;` list
- ✅ Confirmed `crates/gnx-analyzer/Cargo.toml` `[dependencies]` section pattern

## Per-language dispatch table

| # | Lang | tree-sitter crate (try first) | Template | Extensions | Fixture seed concept |
|---|---|---|---|---|---|
| 1 | **Lua** | `tree-sitter-lua` | `c/parser.rs` | `.lua`, `.luau` | `init.lua` (entrypoint), `module_a.lua` (defines a table with methods), `require()` between them |
| 2 | **Solidity** | `tree-sitter-solidity` | `rust/parser.rs` | `.sol` | `Token.sol` (ERC20-like contract), `IToken.sol` (interface), `inherits` + `function` + `event` + `modifier` |
| 3 | **Bash** | `tree-sitter-bash` | `c/parser.rs` | `.sh`, `.bash` | `build.sh` (defines functions), `lib.sh` (sourced), `source ./lib.sh` |
| 4 | **Zig** | `tree-sitter-zig` | `rust/parser.rs` | `.zig` | `main.zig` (entrypoint, has `pub fn main`), `utils.zig` (`@import("utils")`), simple struct |
| 5 | **Move** | `tree-sitter-move` (or git fork if needed) | `rust/parser.rs` | `.move` | `Coin.move` (module with `struct`, `public fun`, `entry fun`) |
| 6 | **Crystal** | `tree-sitter-crystal` | `ruby/parser.rs` | `.cr` | `app.cr` (class with methods), `helper.cr` (`require` between them), inheritance |
| 7 | **Dockerfile** | `tree-sitter-dockerfile` | `c/parser.rs` | `Dockerfile`, `*.dockerfile`, `*.Dockerfile` | one `Dockerfile` with `FROM`, `RUN`, `COPY`, `ENTRYPOINT` |

### Notes per language

**Lua** — queries.scm draft already exists in `docs/superpowers/specs/2026-05-14-gnx-rs-parser-expansion.md` §5.3, use it as a starting point but verify each pattern against actual tree-sitter-lua node types.

**Solidity** — most important captures: `contract_declaration`, `library_declaration`, `interface_declaration`, `function_definition`, `modifier_definition`, `event_definition`, `import_directive`, `inheritance_specifier`. Treat `contract` / `library` / `interface` all as `NodeKind::Class`.

**Bash** — minimal but valuable. Capture: `function_definition` (both `function foo() {}` and `foo() {}` forms), `command` with name=`source` or `.` for imports, top-level `variable_assignment` as const. Bash has no class/heritage.

**Zig** — capture: `function_declaration` (`fn`), `struct_declaration`, `variable_declaration` (`const`), `BuiltinCall` with `@import("...")` for imports. Be careful: Zig's tree-sitter grammar uses `BuiltinCall` for `@import`, not a separate `import` node.

**Move** — capture: `module_definition`, `function_definition`, `struct_definition`, `use_declaration`. Move has no class inheritance; treat module = file-level scope. The `tree-sitter-move` crate may not be on crates.io — check git source `aptos-labs/tree-sitter-move` or similar; fallback to `{ git = "..." }` dependency if needed.

**Crystal** — strong Ruby template fit. Capture: `class`, `module`, `def` (method), `require` (import). Crystal has explicit types, capture return type annotation when present.

**Dockerfile** — capture: `from_instruction` (treat as `@import`, source = image name), `run_instruction` (capture command name as a special const or treat first word as a "call target"), `entrypoint_instruction` / `cmd_instruction` (treat as entry point), `copy_instruction` (file dep). Treat the entire Dockerfile as one File node, no class concept.

## Dispatch mechanics

Each worker runs in its own git worktree (Agent `isolation: "worktree"`). The agent:
1. Reads `docs/superpowers/plans/parser-worker-brief.md`
2. Reads the row of this table for its assigned language
3. Implements the parser end-to-end per the brief's hard constraints
4. Commits on its worktree branch
5. Returns the branch name + a result report

All 7 workers fire in a single parallel batch.

## Phase 2 — Merge plan (after all 7 workers complete)

Worker branches will conflict on three shared files (Cargo.toml, lib.rs, analyze.rs). Merge serially:

```bash
# For each branch returned by an agent:
git checkout main
git merge --no-ff <agent-branch-name>
# Resolve trivial line-add conflicts in Cargo.toml / lib.rs / analyze.rs
# by accepting both sides (just append each new line)
```

After all merges:

```bash
# 1. Confirm builds still pass cleanly with all 7 langs registered
cargo build -p gnx-analyzer
cargo build -p gnx-cli

# 2. Sanity-check by running analyze on each fixture
for L in lua solidity bash zig move crystal dockerfile; do
  target/debug/gnx-cli analyze --repo tests/parity/fixtures/$L/sample_project
done

# 3. One context query per language to confirm symbols resolved
# (per-language, with a known symbol from that language's fixture)
```

If any worker returned a broken build, isolate by checking out their branch alone, debug, fix, re-merge.

## Verification gates

- **Per-worker gate**: cargo build × 2 packages + analyze succeeds + at least one context query returns `status: found`. Hard requirement per worker brief §8.
- **Post-merge gate**: full cargo build of workspace passes; each of 7 fixtures produces a non-empty `graph.bin`; one symbol per language resolves.
- **Out-of-scope**: parity diff against upstream gitnexus (Phase 5 territory in the master spec; not part of this dispatch).

## Cost estimate

- 7 workers × ~$0.17 = ~$1.20 total
- 7 workers ~5–10 min each in parallel = ~10 min wall-clock for the dispatch round
- Phase 2 merge + verification ≈ 30 min by orchestrator

Total: ~$1.20 and ~45 min wall-clock to deliver 7 new languages.
