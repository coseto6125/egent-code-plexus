# Soft-drop function-body locals + scope tag (FUTURE EVAL)

**Status**: future-work backlog. **Not** implemented. Held for re-evaluation when a concrete use case for in-function-body symbols emerges.

**Origin**: Round 78 (2026-05-18) parity audit + 5-Haiku independent panel.

## Context

cgn **hard-drops** function-body / class-body / nested-fn locals in every language parser. queries.scm anchors symbol captures at `(source_file)` / `(module)` / `(mod_item)` direct children. Inside a `function_item` / `def f():` body, `let x = ...` / `x = ...` is invisible to the graph.

The 5-angle panel (token cost, graph correctness, peer tools, use-case grounding, steelman) returned 4 × strong-agree + 1 × qualified-disagree. The steelman raised one principled objection worth holding:

> Hard-drop is **irreversible**. If a future query genuinely needs in-function locals (single-file code summarisation, "where is `cached_result` assigned in this function", scope-aware refactor previews), the data has to be reconstructed from source — there's no soft fallback.

The proposed alternative: **emit locals but tag with `scope: Local`**, default-filter at the query layer, expose via opt-in flag.

## What "soft-drop" would look like

### Storage layer

Add a `scope` field to `Node` (in `crates/cgn-core/src/graph.rs`):

```rust
pub enum Scope {
    Module,    // top-level / mod-direct-child / namespace-direct-child
    Class,     // class-body / impl-body (today's Method / Property scope)
    Function,  // function-body local — currently dropped
    Block,     // nested block inside function body (loop locals etc.)
}

pub struct Node {
    pub uid: StrRef,
    pub name: StrRef,
    pub file_idx: u32,
    pub kind: NodeKind,
    pub span: (u32, u32, u32, u32),
    pub community_id: u16,
    pub scope: Scope,   // NEW
}
```

### Default query filter

Cypher / find / inspect default to `WHERE n.scope IN ['Module', 'Class']`. CLI exposes `--include-locals` to flip.

### MCP exposure

MCP `cgn_search` / `cgn_inspect` accept an optional `include_locals: bool` argument; default `false`. Keeps token cost identical to today.

### Parser-side

queries.scm in each language relaxes the anchor; parser-side annotates the captured node with its tree-sitter ancestor lineage to compute `scope`.

## Why NOT now

| Concern | Severity |
|---|---|
| `Node` rkyv schema bump → every existing `graph.bin` invalidated, `cgn admin index --force` mandatory | High blast radius |
| 14 languages × queries.scm changes + scope-inference per node | Large surgical surface |
| `is_callable` / `is_type` / Pass 2 edge emission / Pass 3 post-process / search ranking / EQUIV map / parity dump tool — all touch NodeKind, scope-aware variants would cascade | Cross-cutting |
| Index file size grows ~2× on .sample_repo (Java/C dominated by function-body locals) | Persistence overhead |
| **No user has asked for in-function symbols.** All current MCP queries (`inspect`, `impact`, `find`, `routes`, `contracts`, `cypher`, `tool-map`) work on module-level or class-level surface. | No demand signal |

The cost is concrete; the upside is hypothetical. **Wait for a real use case** before paying it.

## Trigger conditions for re-evaluation

Implement only when one of these surfaces:

- A user / agent flow concretely needs to ask "what locals exist in function `foo`" via cgn (not "I want it for completeness").
- cgn adds an IDE-like consumer (LSP server, hover, outline view) where document-symbol fidelity matters.
- A single-file summarisation tool wants cgn's structural index instead of raw AST.
- A refactor tool needs scope-aware rename within function bodies.

## When you DO implement

Vertical-slice pilot first:

1. Pick **one** lang (Python — smallest queries.scm, most local-heavy parity gap at 1203).
2. Schema bump + filter + CLI flag + Python queries.scm + Python parser scope inference + tests.
3. Validate on `.sample_repo/Python` — confirm token cost stays flat on default queries, opt-in flag returns full set.
4. Only then fan out to other 13 langs.

Hard-rule: **default-on filter must keep current behaviour byte-stable**. Any consumer not aware of `scope` sees today's graph.

## References

- Memory: `project_drop_locals_is_design.md` — the validated philosophy this defers from.
- queries.scm comments justifying anchors:
  - `crates/cgn-analyzer/src/python/queries.scm:64-74`
  - `crates/cgn-analyzer/src/typescript/queries.scm:47-50`
  - `crates/cgn-analyzer/src/rust/queries.scm:16-29`
- Round 78 parity audit conclusion: top ref_over Variable / Function gaps are design, not bugs.
- Steelman verdict transcript (2026-05-18, agent `a0e42c6a00db23a2a`).
