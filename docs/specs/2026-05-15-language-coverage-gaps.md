# Spec — Closing the ⚠️ Gaps in Language Coverage vs Upstream

**Date**: 2026-05-15
**Status**: Draft (handoff; dispatch in next conversation)
**Related**:
- `fd4f9fe docs(readme): merge LLM positioning + honest scope disclaimer + 14×9 language matrix`
- README Language Matrix (current per-cell delta vs upstream)
- `4e4fb1b feat(python): bind receiver types from local annotations` — Python Constructor Inference prototype to extend

---

## 1. Motivation

The README Language Matrix shows code-graph-nexus has **45 ⚠️ cells** (upstream claims it, we lag) across 14 languages × 9 dimensions. Distribution:

| Dimension | ⚠️ Count | Root Cause |
|---|---:|---|
| Constructor Inference | 13/14 | Python `4e4fb1b` was a prototype; never extended to other 13 langs |
| Frameworks | 10/14 | `framework_confidence.rs` only covers Python (FastAPI/Django/Celery), Rust (Axum/Actix), TS (Express/NestJS), Java (Spring). Missing: JS, Kotlin, C#, Go, PHP, Ruby, Swift, C, C++, Dart |
| Entry Points | 9/14 | `RawRoute` + `RawFrameworkRef` data is collected but never scored into a dedicated "entry point" output |
| Types | 5/14 | C/C++/Swift/Dart/Go type annotation extraction is partial — each language's type AST differs significantly |
| Exports | 4/14 | Go (capital-letter), Ruby (`public`/`private`), C (header decl), Dart (`library` directive) all use conventions not keywords; never implemented |
| Config | 3/14 | C# (csproj/global.json), PHP (composer.json), Swift (Package.swift) missing |
| Named Bindings | 1/14 | Java `import static X.*` + on-demand `import X.*` aren't recorded as aliases |

**Goal**: Close most ⚠️ → ✓ via parallel sub-agent dispatch using git worktrees for isolation.

---

## 2. Task partitioning

22 sub-tasks total, organized into 2 dispatch waves. Each task gets:
- A dedicated git worktree at `~/code-graph-nexus/.claude/worktrees/task-<id>/`
- A feature branch `feat/<scope>` based on main HEAD
- A sub-agent (general-purpose) with the brief in §4
- Acceptance criteria (§3) verified before merge

### 2.1 Wave 1 (8 parallel agents, no shared crate state)

| ID | Scope | Languages × Dimension | Primary files | Est. LOC |
|---|---|---|---|---|
| **A1** | Ctor: TypeScript / JavaScript | `this.foo()` / `obj.method()` | `crates/cgn-analyzer/src/typescript/parser.rs`, `.../javascript/parser.rs` | ~150 |
| **A2** | Ctor: Java / Kotlin / C# | `this.` + `base.` / `super.` | `.../java/parser.rs`, `.../kotlin/parser.rs`, `.../c_sharp/parser.rs` | ~200 |
| **A3** | Ctor: Go / Rust | struct receiver / `impl` receiver | `.../go/parser.rs`, `.../rust/parser.rs` | ~150 |
| **A4** | Ctor: PHP / Ruby | `$this->` / `self.` | `.../php/parser.rs`, `.../ruby/parser.rs` | ~120 |
| **A5** | Ctor: Swift / C / C++ / Dart | `self.` / `obj->` / `obj.` | 4 parser.rs files | ~200 |
| **E** | Exports: Go / Ruby / C / Dart | naming convention / `library` directive | 4 parser.rs files | ~100 |
| **G+F1** | Java Named + C# Config | `import static`, csproj, global.json, NuGet.config | `.../java/parser.rs`, `crates/cgn-cli/src/config_parser.rs` | ~120 |
| **C** | Cross-lang Entry Point scorer | combine `RawRoute` + `RawFrameworkRef` + `main()` detection → scoring | `crates/cgn-analyzer/src/entry_points.rs` (NEW) | ~200 |

**Wave 1 total**: ~1240 LOC across 8 agents. Estimated wall-clock: 30-60 min if agents run truly in parallel.

### 2.2 Wave 2 (7 parallel agents, base on merged Wave 1)

| ID | Scope | Primary files | Est. LOC |
|---|---|---|---|
| **B1** | Frameworks: JS (Express, Hapi) | `crates/cgn-analyzer/src/framework_helpers.rs` + `.../javascript/parser.rs` | ~180 |
| **B2** | Frameworks: Kotlin (Ktor) / C# (ASP.NET Core) | `framework_helpers.rs`, `.../kotlin/parser.rs`, `.../c_sharp/parser.rs` | ~200 |
| **B3** | Frameworks: PHP (Laravel) / Ruby (Rails) / Go (gin / echo) | 3 parser.rs files + `framework_helpers.rs` | ~250 |
| **B4** | Frameworks: Swift (Vapor) / C++ (Crow) / Dart (shelf) | 3 parser.rs files + `framework_helpers.rs` | ~200 |
| **D1** | Types: Go (struct fields + signatures) | `.../go/parser.rs` | ~100 |
| **D2** | Types: Swift / Dart (declared + generics) | 2 parser.rs files | ~150 |
| **D3** | Types: C / C++ (typedef + function signatures) | 2 parser.rs files | ~150 |
| **F2+F3** | Config: PHP composer.json + Swift Package.swift | `crates/cgn-cli/src/config_parser.rs` | ~120 |

**Wave 2 total**: ~1350 LOC. **Must base on merged Wave 1** because Ctor receiver-type binding is a precondition for framework_ref resolution.

---

## 3. Acceptance criteria (uniform across all tasks)

Each ⚠️ cell that the task converts to ✓ must satisfy:

1. **Parser code** populates the corresponding field on `RawNode` / `RawEdge` / `EmbeddingProfile` etc. Concretely:
   - **Constructor Inference**: receiver type bound on method call sites; reference: how `crates/cgn-analyzer/src/python/parser.rs` does it after `4e4fb1b`
   - **Frameworks**: `framework_helpers.rs` registers a detector; tests in `tests/<lang>_framework_*.rs` pass
   - **Entry Points**: emit `EntryPoint` node kind (or scored attribute on existing nodes); covered by integration test
   - **Types**: `type_annotation` field set on parameters / returns / variables for declared types
   - **Exports**: `is_exported: true` set on symbols matching the language's export convention
   - **Config**: parsed file emitted as `Config` node with kind discriminator (`csproj` / `composer-json` / `swift-package` etc.)
   - **Named Bindings**: `alias` field set on Import-shaped types (Java static imports populate the binding map)

2. **At least one test** per dimension covered. Place under `tests/<lang>_<dim>.rs` or extend an existing fixture. Test must:
   - Construct a small parse input demonstrating the feature
   - Assert the relevant field is populated correctly
   - NOT duplicate logic under test (call actual parser, not reimplement)

3. **`cargo test -p cgn-analyzer`** passes in the task's worktree.

4. **`cargo check --release -p code-graph-nexus`** passes (downstream CLI still compiles).

5. **No regression** in existing tests (full suite must remain green).

6. **README Language Matrix updated** in the same commit: the relevant ⚠️ cell flipped to ✓.

7. **Conventional commit message**:
   - `feat(<lang>): <scope> (closes ⚠️ <dimension> for <lang>)`
   - Example: `feat(go): receiver-type binding on method calls (closes ⚠️ Ctor for Go)`

---

## 4. Per-task agent brief template

Each sub-agent gets a self-contained prompt of this shape (filled in per task):

```
You are implementing <Task ID>: <scope description>.

**Worktree**: /home/enor/code-graph-nexus/.claude/worktrees/task-<id>/
**Branch**: feat/<scope> (already created from main)
**Goal**: Close ⚠️ cell(s) <list> in the README Language Matrix.

**Reference implementation** (read first):
- `crates/cgn-analyzer/src/python/parser.rs` — the Python parser
  established the pattern for <relevant dimension> in commit 4e4fb1b.
- `crates/cgn-analyzer/src/types.rs` — the shared field schema
  every parser must populate.

**Files you'll modify**:
- <file 1>
- <file 2>
- README.md and README_zh-TW.md — flip the ⚠️ cell to ✓
- tests/<lang>_<dim>.rs — add at least one test

**Acceptance criteria**:
- (copied from §3 above)

**Out of scope**:
- Don't touch other languages' parsers
- Don't refactor the shared infrastructure (types.rs, builder.rs) unless
  strictly necessary; if you need a new field, add it as optional with
  serde default
- Don't update other ⚠️ cells in the README — each task is responsible
  ONLY for the cells in its scope

**Commit + report**:
Commit your work with the message format in §3.7. Don't push to remote.
Report back: (1) list of cells flipped to ✓, (2) test command(s) run
showing pass, (3) any unexpected blockers.

**Read for full context**:
docs/specs/2026-05-15-language-coverage-gaps.md
```

---

## 5. Dispatch mechanics

### 5.1 Worktree setup script (run in main repo before dispatching agents)

```bash
TASKS_W1=("a1-ctor-ts-js" "a2-ctor-java-kotlin-csharp" "a3-ctor-go-rust" \
          "a4-ctor-php-ruby" "a5-ctor-swift-c-cpp-dart" \
          "e-exports-go-ruby-c-dart" "g-f1-java-named-csharp-config" \
          "c-entry-point-scorer")

for task in "${TASKS_W1[@]}"; do
    git worktree add -b "feat/${task}" \
        ".claude/worktrees/task-${task}" main
done
```

### 5.2 Agent dispatch order

**Wave 1** — dispatch all 8 in a single `Agent` tool batch (multiple tool_use blocks in one assistant message). Each agent gets its filled brief from §4 + its assigned worktree path.

**Sync point**: Wait for all 8 Wave 1 agents to complete. Review reports. Manually merge each `feat/<task>` branch into main (or open a PR per task). Resolve any README conflicts (multiple agents each flip different ⚠️ cells — merge order matters).

**Wave 2** — after Wave 1 merged, run the worktree setup again with Wave 2 task IDs (`b1-fw-js`, `b2-fw-kotlin-csharp`, `b3-fw-php-ruby-go`, `b4-fw-swift-cpp-dart`, `d1-types-go`, `d2-types-swift-dart`, `d3-types-c-cpp`, `f2-f3-config-php-swift`). Dispatch all 7 in parallel.

### 5.3 README conflict resolution

Each agent flips its own ⚠️ → ✓ cells. When merging 8 branches sequentially into main, the README table will conflict at the same row multiple times. Strategy:

1. Merge agents in dimension order (Ctor first, then Exports, then Named/Config, then Entry Points)
2. Use `git checkout --theirs README.md README_zh-TW.md` only if the agent strictly flipped its assigned cells; verify by inspecting the diff
3. If conflicts span more than the assigned cells, the agent overstepped — revert and re-dispatch with stricter scope

### 5.4 Cargo target cache

Each worktree has its own `target/` dir. First build per worktree is ~1-2 min (cold). Subsequent builds in the same worktree are seconds. **Total cold-build cost across 8 worktrees ~10-15 min** parallelized (Rust workspaces can build in parallel; disk I/O is the bottleneck).

If disk space matters, set `CARGO_TARGET_DIR=/tmp/cgn-build-cache-<task>` per agent so caches share when possible — but this loses task isolation. Default: keep per-worktree target dirs.

---

## 6. Risk register

| Risk | Likelihood | Mitigation |
|---|---|---|
| Two Wave-1 agents touch overlapping files (e.g. shared helper) | Low | Wave-1 tasks were selected to be parser-local. `framework_helpers.rs` is only touched in Wave 2 |
| Wave-2 `framework_helpers.rs` 4-way conflict (B1-B4 all register helpers) | High | Refactor `framework_helpers.rs` into per-language modules BEFORE Wave 2 dispatch, OR serialize Wave 2 B-tasks (B1 → B2 → B3 → B4) |
| `types.rs` field schema changes break other parsers | Medium | New fields MUST be `Option<T>` with `#[serde(default)]`. No required-field additions |
| Acceptance test missing for some cells | Medium | Each agent's brief explicitly lists which cells they're responsible for; reviewer (next-conversation orchestrator) cross-checks against `tests/` |
| Agent claims "✓" without real evidence | Medium | Spec §3 requires a passing test per dimension. Reviewer verifies `cargo test -p cgn-analyzer -- <test_name>` actually runs the new test |
| Constructor Inference patterns vary per-lang and agents diverge | High | All A-task agents must Read `crates/cgn-analyzer/src/python/parser.rs` + commit `4e4fb1b` as the reference template. Field names + edge kinds align with Python's |
| Entry Point scorer (Task C) needs cross-cutting refactor | Medium | Brief explicitly says: emit `EntryPoint` node kind as new addition; do NOT refactor existing route/framework extraction. Pure consumer of existing data |

---

## 7. Out of scope (explicitly NOT in this spec)

- **N1. multilingual-e5-small embedding model loader** — owned by `docs/specs/2026-05-15-embedding-profile-freeze.md`. Distinct workstream
- **N2. Adding new languages beyond the 14** — the 17 extra Rust providers (Bash, Crystal, Cairo, etc.) are structural-only by design
- **N3. Removing the `△` legend** — current README uses `✓ / ✅ / ⚠️ / —` only. The internal audit's `△` was a pre-publication distinction; the public table doesn't need it
- **N4. Replacing parser implementations** — this spec is about *gap closure*, not parser quality improvements. Existing ✓ cells stay as-is
- **N5. Cross-language refactor of `types.rs`** — keep the schema additive

---

## 8. Done criteria for the whole effort

When both waves complete and merge, the README Language Matrix should show:
- **Wave 1 closes**: ~16 ⚠️ cells (Ctor 13 cells across all langs, Exports 4 cells, Named 1 cell, Config 1 cell — Java/C# tasks, Entry Points 9 cells via scorer)
- **Wave 2 closes**: ~22 ⚠️ cells (Frameworks 10 cells, Types 5 cells, Config 2 cells — PHP/Swift)
- **Total**: ~38 ⚠️ cells flipped to ✓ out of 45 starting count
- **Remaining ⚠️**: ~7 cells, likely concentrated in edge cases (e.g. Constructor Inference for languages without static typing like Ruby may stay `△` even after work, since runtime dispatch makes full receiver binding undecidable)

A follow-up audit (similar to the Explore agent run that produced the current matrix) verifies the matrix accuracy after Wave 2 ships.

---

## 9. Anticipated open questions during dispatch

- **Q1**. How does the Entry Point scorer decide weight? — Initial proposal: routes → weight 1.0; `main()` → 0.9; framework_ref decorator → 0.8; public exported symbol with imports referencing it externally → 0.5. Final tuning per real-repo evaluation.
- **Q2**. For Frameworks tasks (B1-B4), should we vendor framework signatures in code or in a TOML/YAML config? — Recommendation: code (Rust match arms in `framework_helpers.rs`), keeps lookups branch-predictable and avoids serialization overhead.
- **Q3**. Should `EmbeddingProfile` (from embedding-profile-freeze spec) include the closed-gaps version? — Likely yes; bumping the analyzer's behavior fingerprint after Wave 2 should invalidate stale embeddings. Decide during embedding-profile-freeze implementation.

---

## Appendix A — Current ⚠️ cell inventory (snapshot)

Snapshot from README at commit time. Each cell will be flipped to ✓ by the noted task.

```
TS:      Ctor ⚠️ (A1)
JS:      Ctor ⚠️ (A1), Frameworks ⚠️ (B1)
Python:  (none)
Java:    Named ⚠️ (G), Ctor ⚠️ (A2), Entry ⚠️ (C)
Kotlin:  Ctor ⚠️ (A2), Frameworks ⚠️ (B2), Entry ⚠️ (C)
C#:      Ctor ⚠️ (A2), Config ⚠️ (F1), Frameworks ⚠️ (B2), Entry ⚠️ (C)
Go:      Exports ⚠️ (E), Types ⚠️ (D1), Ctor ⚠️ (A3), Frameworks ⚠️ (B3), Entry ⚠️ (C)
Rust:    Ctor ⚠️ (A3), Entry ⚠️ (C)
PHP:     Ctor ⚠️ (A4), Config ⚠️ (F2), Frameworks ⚠️ (B3)
Ruby:    Exports ⚠️ (E), Ctor ⚠️ (A4), Frameworks ⚠️ (B3)
Swift:   Types ⚠️ (D2), Ctor ⚠️ (A5), Config ⚠️ (F3), Frameworks ⚠️ (B4), Entry ⚠️ (C)
C:       Exports ⚠️ (E), Types ⚠️ (D3), Ctor ⚠️ (A5), Frameworks ⚠️ (B4), Entry ⚠️ (C)
C++:     Types ⚠️ (D3), Ctor ⚠️ (A5), Frameworks ⚠️ (B4), Entry ⚠️ (C)
Dart:    Exports ⚠️ (E), Types ⚠️ (D2), Ctor ⚠️ (A5), Frameworks ⚠️ (B4), Entry ⚠️ (C)
```

---

## Appendix B — How to verify a closed cell

Quick sanity check after each agent merge:

```bash
# Confirm cell flipped in README
grep -A 1 "<lang>" README.md | grep -c "⚠️"   # should drop by N matching task

# Confirm parser emits the field
grep -rn "<field>: \|<field>:Some" crates/cgn-analyzer/src/<lang>/parser.rs

# Confirm test exists
ls tests/<lang>_*.rs
cargo test -p cgn-analyzer <lang>::<test_name> -- --nocapture

# Confirm no regression
cargo test -p cgn-analyzer
cargo check --release -p code-graph-nexus
```

---

## 10. Implementation note for next conversation

When you start the next session:

1. Read this spec from start to finish (no shortcut — full context matters)
2. Run `git log --oneline -10` to see if anything's changed in main since this spec was written
3. Verify Appendix A's ⚠️ inventory matches the current README — if cells have moved, update the task assignments
4. Set up Wave 1 worktrees per §5.1
5. Dispatch all 8 agents in a single message with parallel `Agent` tool calls per §5.2
6. While Wave 1 runs, draft the Wave 2 worktree script (don't dispatch yet)
7. On Wave 1 complete: review each agent's commit + test output; merge in dimension order (§5.3)
8. README rebuild matrix manually if conflicts get hairy
9. Wave 2 dispatch
10. Final audit (re-run the Explore agent prompt that produced the current matrix) to verify all flipped cells stuck
