# BlindSpot cross-lang rollout — design spec

Status: design / partial-impl (P0 landed in this PR; P1–P7 owned by FU-001 session)
Owner: cross-cutting (verdict layer in this PR; per-lang emitters in FU-001)
References:
- `crates/ecp-analyzer/src/python/parser.rs:53-78` — Python BLIND_SPEC
- `crates/ecp-analyzer/src/indirect_dispatch.rs:160-163` — Rust dispatch detection rules
- `crates/ecp-core/src/graph.rs:506` — `BlindSpotRecord`
- `crates/ecp-core/src/graph.rs:527` — `CallMeta`
- `crates/ecp-cli/src/commands/review/verdicts.rs` — verdict layer
- `FOLLOWUPS.md` FU-001 (on `main`)

## Problem

The `BLINDSPOT_IN_DIFF_REGION` review verdict was emitted purely from
`graph.blind_spots`, which only the Python parser populates (6 kinds:
`python-eval` / `-exec` / `-compile` / `-dynamic-import` /
`-builtin-import` / `-cross-getattr`). For Java / Kotlin / C# / Go / Rust
/ PHP / Ruby / Swift / TS / JS / C / C++ / Dart PRs the verdict was
silent — exactly the codebases where indirect dispatch matters most.

The naïve fix ("add a BlindSpot emitter per lang") underestimates cost
(~1700 LOC × 14 lang per FU-001) and overlooks that
`indirect_dispatch.rs` *already* captures 6 langs' indirect call sites,
just through a different data path (`CallMeta.flags`, not
`BlindSpot`).

## Two-pipeline model

| Pipeline | Carries | Semantic |
|---|---|---|
| `BlindSpot` (parser → `blind_spots` Vec) | "**target is undecidable**" — `eval` / `exec` / dynamic import / reflection where ecp **cannot** enumerate possible callees. Graph **has no Calls edge** for the unknown target. | LLM should: read source, reason manually about runtime targets. Refactoring guarantees are bounded by the BlindSpot's hint. |
| `CallMeta` flags (`indirect_dispatch.rs` → `call_metas` Vec) | "**target is one-of-N known impls/callbacks**" — vtable / `&dyn Trait` / `Fn`-typed parameter. Graph **has Calls edges** to all candidate impls (via Implements / Extends traversal). | LLM should: enumerate all impls of the dispatched type (already in graph), verify each still satisfies the contract. |

**Critical**: these are NOT interchangeable. Merging them would conflate
"no data" with "scattered data" — the LLM needs the distinction to know
whether to ASK (BlindSpot) or to SCAN ALL IMPLS (CallMeta). The verdict
layer surfaces both with separate kinds; downstream consumers must treat
them as different actions.

## What this PR ships (P0)

A new VerdictKind `INDIRECT_DISPATCH_IN_DIFF_REGION` that derives from
existing `CallMeta` entries:

```rust
// New in symbols.rs UnknownBucket:
pub indirect_dispatches_in_diff_region: Vec<IndirectDispatchRef>,

// Where IndirectDispatchRef carries:
//   path, line (caller fn start), kind ("dynamic_dispatch" | "callback"),
//   dispatch_type (e.g. "Box<dyn Trait>"), caller (fn name)
```

Test-code callers (`FunctionMeta::FLAG_TEST` set) are filtered out at
collection time — verdict noise on PRs that touch test scaffolding stays
under control.

Severity: `Warn`. Targets exist in graph, so it's not `Risk` (no silent
break), but a refactor still needs LLM attention because direct-caller
chasing won't enumerate the actual runtime target.

### Coverage after P0

| Lang | Before P0 (Python-only) | After P0 (verdict via CallMeta) | After P1–P7 (BlindSpot emitter) |
|---|:---:|:---:|:---:|
| Python | ✓ (eval/exec) | ✓ (+ indirect via CallMeta) | ✓ |
| TypeScript | — | ✓ (dyn dispatch + callback) | ✓ (+ eval / `Function()`) |
| JavaScript | — | ✓ | ✓ (+ eval / `Function()`) |
| Rust | — | ✓ (dyn Trait + Fn callbacks) | ✓ (+ libloading / transmute) |
| C | — | ✓ (function pointers) | ✓ (+ dlsym handle invocations) |
| C++ | — | ✓ (virtual + funcptr) | ✓ (+ std::function from unknown) |
| Java / Kotlin / C# | — | — | ✓ (reflection / `Method.invoke`) |
| Go | — | — | ✓ (reflect.Value.Call / plugin) |
| PHP / Ruby | — | — | ✓ (variable functions / send) |
| Swift / Dart | — | — | ✓ (selector / Function.apply) |

P0 lifts coverage from 1/14 → 7/14 with zero new parser work.

## P1–P7 design constraints (for FU-001 owner)

### Constraint 1: BlindSpot vs CallMeta — emit the right one

Per-lang `BLIND_SPEC` table entries are for cases where the **target is
undecidable**. Examples that MUST go through `BlindSpot`:
- TS `eval("code")`, `new Function("code")`, `import(<var>)`
- Java `Method.invoke()`, `Class.forName(<runtime-string>)`
- Go `reflect.Value.Call()`, `plugin.Open()`
- Rust `transmute::<_, fn(...)>(ptr)`, `libloading::Library::get()`
- PHP `eval()`, `call_user_func($var)` (variable, not literal)
- Ruby `eval()`, `send(<var>)` (variable, NOT `send(:literal_symbol)`)
- Swift `perform(Selector("name"))`, `NSClassFromString()`
- Dart `dart:mirrors`, `Function.apply` with non-literal fn

Examples that MUST NOT emit BlindSpot — they belong to the
`indirect_dispatch.rs` (CallMeta) path:
- Rust `dyn Trait` (already covered)
- C++ virtual method calls (already partially covered)
- Java/Kotlin/C# interface method calls (NEW work needed in
  `indirect_dispatch.rs`, NOT a BlindSpot)
- Go interface method calls (NEW work needed in `indirect_dispatch.rs`)

Items previously listed under P-phase tables that **do not belong** in
either pipeline:
- Rust `Any::downcast` — typed introspection, target known at compile
  time
- Go `text/template.Execute` — template render, not function dispatch
- Ruby `method_missing` — class-level definition, not a call site
- `dlsym` itself — handle creation, not invocation; the invocation site
  later is what matters

### Constraint 2: literal vs variable argument

For TS `import("./foo")` the URL is a literal and already resolves via
the Imports edge — do NOT emit BlindSpot. Only `import(varName)` where
the argument is a non-string-literal expression should emit. This
distinction is per-call-site argument-kind inspection in tree-sitter;
each lang's parser must check the argument node type before pushing the
BlindSpot record.

Same rule applies to Ruby `send(:method)` vs `send(var)`, PHP
`call_user_func("known")` vs `call_user_func($var)`, Dart
`Function.apply(known, ...)` vs `Function.apply(<expr>, ...)`.

### Constraint 3: span convention for multi-line chains

`Class.forName(name).getDeclaredMethod(...).invoke(...)` spans 3 calls
on potentially 3 lines. The BlindSpot span should be the OUTERMOST call
expression (the whole `invoke` call), not the innermost (`forName`).
Rationale: that's the call that actually executes unknown code; LLM
needs to see that exact site.

For Python's existing emit sites this isn't an issue (single-call
patterns), but Java/C# reflection chains will hit it. Spec: capture the
outermost `call_expression` node that owns the dispatch.

### Constraint 4: `is_test` filtering

`BlindSpotRecord` struct does NOT carry an `is_test` flag. The current
P0 implementation filters at verdict-collection time via `FunctionMeta`
(works for indirect dispatch because the call site has a caller fn). For
true BlindSpot records (file-level eval calls outside any function),
this filter doesn't apply directly.

Options for P1+:
- (a) Add `is_test: bool` to `BlindSpotRecord` and populate from the
  containing file's category. **Recommended** — most natural place.
- (b) Filter at verdict layer by walking up from the BlindSpot's line to
  find a containing FunctionMeta. More complex, doesn't handle module-
  level emitters.

If (a) is chosen, this is a schema change requiring a `graph.bin`
discriminant bump — coordinate with parser owners before landing.

### Constraint 5: schema introspection requirement

After P1–P7 land, `ecp schema blindspots` (a yet-to-exist subcommand)
must list:
- Per-lang: `implemented | partial | none` for BlindSpot emitter
- Per-lang: `implemented | partial | none` for CallMeta indirect-
  dispatch detection
- All known kinds across langs with their hint patterns

Without this, an LLM seeing `INDIRECT_DISPATCH_IN_DIFF_REGION` empty in a
Java PR cannot distinguish "no indirect dispatch in this diff" from "our
Java parser doesn't detect it yet" — silent gap precisely where it
matters.

This subcommand is out of scope for the current PR but **must** ship
before P1–P7 close. Suggested shape:

```json
{
  "languages": [
    {
      "name": "Python",
      "blindspot_emitter": "implemented",
      "indirect_dispatch": "implemented",
      "blind_kinds": ["python-eval", "python-exec", ...]
    },
    { "name": "Java",  "blindspot_emitter": "none", "indirect_dispatch": "none", ... },
    ...
  ]
}
```

### Constraint 6: shared dispatcher skeleton

Python's per-lang BLIND_SPEC + capture-index dispatch is ~80 LOC of
mechanical wiring. After P1 + P2 (TS/JS + JVM family) are written, the
shared part should be extracted to
`crates/ecp-analyzer/src/common/blind_spot_dispatcher.rs`:

```rust
pub fn dispatch_blind<'a>(
    cap_idx: u32,
    idx_table: &'a [u32],          // per-lang
    spec_table: &'a [(&str, &str)],// per-lang
    node: &tree_sitter::Node,
    file_path: &Path,
    out: &mut Vec<BlindSpot>,
);
```

Each lang then provides just the tree-sitter query + spec table. Per-lang
landing cost drops from ~80 LOC to ~30 LOC. Do NOT extract before P1+P2
exist — single-data-point abstraction is the canonical premature
generalization.

## Non-goals

- This spec does NOT extend `indirect_dispatch.rs` to Java/Kotlin/C#/Go.
  Those need separate parser-side work (the constraint-2 "interface
  method calls" case). Tracked in FU-001 as Type-2 BlindSpot.
- This spec does NOT design `ecp schema` introspection beyond the
  constraint-5 sketch. That's a follow-up subcommand.
- No new graph schema fields ship in P0 (only verdict-layer wiring on
  existing graph data).

## Test strategy

P0: integration test on a Rust trait-object fixture (this PR ships
`review_verdicts_indirect_dispatch_test.rs`). Confirms the wiring;
per-lang detection has its own coverage in `indirect_dispatch.rs`'s
existing test suite.

P1–P7: each phase ships with a per-lang BlindSpot fixture in the
existing `crates/ecp-analyzer/tests/<lang>_<dimension>.rs` pattern.
CLAUDE.md's 14-lang rule applies — single-lang PRs for a multi-lang
change get rejected. Suggested table-driven runner shape:

```rust
const CASES: &[(&str, &str, &[&str])] = &[
    ("python", "eval('x')",         &["python-eval"]),
    ("typescript", "eval('x')",     &["ts-eval"]),
    ("java", "Method m; m.invoke()",&["java-method-invoke"]),
    ...
];
```

One driver, ~70 cases, ~150 LOC total — beats 14 hand-written test
files.
