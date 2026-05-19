# Macro / preprocessor invocation: cross-lang kind spec

> Status: design — surfaces an existing inconsistency, proposes a stance.
> No parser changes attached. Raised as part of parity-r3 (PR #152
> follow-up) when ref-gitnexus's macro-invocation handling came up while
> auditing Cpp `Function-10` / `Variable-2726` ref_over candidates.

## The shape

A "macro invocation" is a call-form identifier expression at
file / namespace / class scope whose target resolves to a macro
definition (or a preprocessor mechanism that looks like one to the
consumer). Examples in `.sample_repo` today:

| Lang | Site | Source | Semantic effect |
|------|------|--------|-----------------|
| Cpp  | `tests/src/unit-udt_macro.cpp:42`          | `NLOHMANN_DEFINE_TYPE_INTRUSIVE(person, name, age)` | Generates `to_json` / `from_json` friend functions on `person` |
| Cpp  | `tests/src/unit-assert_macro.cpp:8`        | `DOCTEST_CLANG_SUPPRESS_WARNING("-Wunused")`        | Emits a `_Pragma(...)` line, no symbol effect |
| C    | `src/cluster_legacy.c:42`                  | `IMPLEMENT_DYNCREATE(CClass, CParent)`              | Generates a `static` runtime-class registration |
| Cpp  | `src/Qt/*.cpp`                             | `Q_OBJECT`                                          | Generates meta-object table; load-bearing for signals/slots |
| Rust | various                                    | `lazy_static! { static ref X: T = ...; }`           | Generates a thread-local lazy static |
| Rust | various                                    | `bitflags! { struct F: u32 { ... } }`               | Generates a bitflag struct |
| Python | various                                  | `@dataclass\nclass Foo:`                            | Adds `__init__` / `__repr__` to the decorated class |

The ones LLM consumers care about are the **load-bearing** ones —
they generate new symbols, change visibility, or wire signals.
The ones they don't care about are **pragma-shaped** —
diagnostic suppression, alignment hints, optimization barriers —
that produce no semantic symbol.

## Current state

### gnx-rs (no emission)

gnx-rs does not emit anything for macro **invocations**, only for
macro **definitions** (`#define X` → `NodeKind::Macro`). The C / Cpp
queries.scm capture `preproc_def` and `preproc_function_def` for the
declaration site but have no `call_expression` query gated on
known-macro identifiers.

This is a coverage gap — but a deliberate one until now, because:

- Tree-sitter has no way to know that `Foo(...)` resolves to a
  preprocessor macro vs. a regular function (resolution requires
  the preprocessor, which neither tree-sitter nor gnx implements).
- Any rule we pick (allowlist by name, allcaps heuristic, etc.) is
  fuzzy and will misfire on at least one corpus.

### ref-gitnexus (inconsistent emission)

ref-gitnexus does emit macro invocations but classifies them
inconsistently:

| Macro | ref kind | Why |
|-------|----------|-----|
| `NLOHMANN_DEFINE_TYPE_INTRUSIVE(...)` | `Function` | Looks call-shaped, treated as a function call expression |
| `DOCTEST_CLANG_SUPPRESS_WARNING(...)` | `Variable` | Parsed as a top-level expression statement, ends up under variable taxonomy |
| `Q_OBJECT` (no args) | not emitted | No call-shape, doesn't match either rule |

This is what the parity report surfaces as 8 Cpp `Function-*` and 4-7
Cpp / C `Variable-*` ref_over rows for these macro families. None
are gnx-side bugs — gnx is consistently silent — but they show up as
"ref_over" because ref labels them inconsistently and the EQUIV map
can't pair `Macro ↔ Function` without overpairing.

## Decision

**Stay silent for the macro-invocation case. Track each load-bearing
macro family separately as Tier-2 framework support.**

Justification:

1. **No clean kind exists.** A macro invocation is neither a Function
   (no callable to navigate to — the body is preprocessor-substituted
   inline) nor a Variable (no binding survives after preprocessing).
   Forcing it into one of these labels poisons searches: an LLM that
   sees `Function:NLOHMANN_DEFINE_TYPE_INTRUSIVE` then tries to follow
   `gnx inspect` will get the macro's generated-code surface, not a
   useful answer.

2. **The signal-to-noise ratio is bad without a curated allowlist.**
   `DOCTEST_CLANG_SUPPRESS_WARNING` is noise (no symbol effect);
   `Q_OBJECT` is signal (load-bearing for Qt). Distinguishing them
   requires a per-macro decision, not a syntactic one. gnx's
   `frameworks.scm` per-lang file is the right home for that, NOT
   the generic parser query.

3. **Parity penalty is small.** Cpp Function-10 + Variable ~12 +
   C Macro-2 ≈ ~25 rows on 25 k files. The Variable-2726 mass
   reported in the same per-lang summary is the locals-drop, an
   unrelated category. Removing 25 rows by adopting one of ref's
   labels would force us to pick a wrong label.

## What we DO

| Path | Action |
|------|--------|
| Generic macro invocations (no framework signal) | Stay silent. Document this stance in `c/queries.scm` and `cpp/queries.scm`. |
| Load-bearing macros for known frameworks | Add to per-lang `frameworks.scm` with the right shape (e.g., Qt `Q_OBJECT` → `RawFrameworkRef` with `qt-meta-object` reason). |
| Rust `lazy_static! { ... }` / `bitflags! { ... }` | Already partially handled by tree-sitter-rust's macro_invocation node; defer to the resolver's "structural-macro" pass when added. |
| Python decorators on classes | Already captured by the python parser's `@decorator` capture and surfaces as `RawFrameworkRef` for dataclass / pydantic-like cases. No change. |

## What we DON'T do

- **Do not** add a generic `NodeKind::MacroCall`. The empty kind
  would either need a population rule (and we just argued that
  rule doesn't exist cleanly) or it stays empty (and we've added
  a kind variant for no payoff, paying the `FileCategory`-style
  cost on every consumer).
- **Do not** label macro invocations as `Macro`. `NodeKind::Macro`
  is already taken by `#define` declarations. Reusing it for
  invocations collapses two distinct shapes into one kind, which
  is the exact failure mode the current `Function` / `Variable`
  inconsistency on ref-gitnexus exhibits.
- **Do not** match by `[A-Z_]+` allcaps heuristic in the parser.
  C++ template-parameter idents (`ARG`, `T1`, `RESULT`) are
  allcaps but aren't macros; the same heuristic would mis-classify
  them. Stick to per-framework allowlists in `frameworks.scm`.

## Parity expectations after this stance

The Cpp / C ref_over rows in the table below stay where they are —
classified as `real_ref` in the parity aggregator. Reviewers can
verify each one is a macro invocation against the source file and
treat the row as "macro family not yet on the framework allowlist."

```
Cpp ref_over (macros):
  Function-10 (NLOHMANN_*, DOCTEST_*)
  Variable-12 (DOCTEST_CLANG_SUPPRESS_WARNING)
  Macro-2     (TSDN_NULL, DOCTEST_CMP_GE — these are #define macros gnx-rs DOES support,
               separate bug, file under "C/Cpp queries.scm coverage gap")

C ref_over (macros):
  Macro-2     (RO_MUTEX_CTL_GEN, atom — same #define-coverage gap)
```

## Open follow-ups

- **Qt `Q_OBJECT` → framework_ref**. Tracked as a Tier-2 Qt support
  ticket; not in scope here.
- **nlohmann_json `NLOHMANN_DEFINE_TYPE_INTRUSIVE` → framework_ref**.
  Generates serialization methods on the named struct; surfacing it
  as a binding from the struct to "serialization" would help the
  LLM answer "is `person` JSON-serializable?" without reading the
  macro body.
- **`#define` macros gnx-rs misses (the 4 Macro-2 ref_over rows
  above)**. Investigate whether they're inside `#if` guards or other
  preprocessor branches that the tree-sitter query doesn't enter.

Related: [[soft-drop-locals-future-eval]] (similar
"explicit-non-emission" stance for function-body locals).
