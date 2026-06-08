# Cypher `EXISTS` / `IS NULL` predicates + error guidance + telemetry attribution

**Date:** 2026-06-08
**Branch:** `feat/cypher-exists-predicate`
**Status:** approved, pre-implementation

## Problem

CLI telemetry (`ecp usage`) shows the `cypher` subcommand at **41.7% error
rate** — the worst of any verb. All failures are `user-input`: the consuming
LLM writes openCypher that ecp's hand-written subset engine doesn't accept.

Pulling the raw failing queries (`ecp usage --failures --all`) and classifying
by *intent*, not syntax, is decisive:

| Intent | # queries | Syntaxes the LLM tried |
|---|---|---|
| **Find orphans** (symbol with no caller / no implementor) | **8 / 10** | `NOT EXISTS(pattern)` ×5, `OPTIONAL MATCH … IS NULL` ×1, `COUNT(pattern)=0` ×1, `LEFT JOIN … IS NULL` ×1 |
| All trait implementations | 1 | label union `Class\|Struct\|Enum` (already supported) |
| Node edge-types | 1 | `CALL … YIELD` |

The LLM reaches for 5 different syntaxes to express *one* semantic — "find
things with no incoming edge". These are the LLM's most intuitive phrasings;
ecp rejecting them is ecp's gap (priority #2: LLM helpfulness), not the LLM's
error. The fix is to support the **semantic primitives** the LLM actually
needs, not to chase full openCypher.

## Non-goals (rejected against the LLM-utility gate / correctness)

- **Full openCypher write clauses** (`CREATE`/`SET`/`MERGE`/`DELETE`) — no
  meaning on a read-only, index-time-built code graph.
- **`CALL … YIELD` procedure framework** — 1 failing query, has an equivalent
  in `ecp inspect`. Routed via an error hint instead.
- **pattern-in-aggregate** `COUNT((other)-[:Calls]->(f))` — has a genuine
  **correctness problem**, not just YAGNI:
  1. The aggregate-arg evaluator (`executor.rs:633-639`) calls
     `eval_expr(arg, binding)` once per existing binding and feeds the scalar
     to an accumulator. A pattern with an *unbound* variable (`other`) violates
     this "binding → scalar" contract — it would require running a nested
     pattern-match inside `eval_expr`, which currently returns `Value::Null`
     for unknown vars (`executor.rs:1312`).
  2. `COUNT(pattern)` is **not legal openCypher** — Neo4j requires
     `COUNT { pattern }` or `size([pattern | x])`. Implementing it means
     inventing semantics for a syntax no spec defines.
  3. It overlaps `EXISTS` (for the `=0` case) but with different optimization
     characteristics (count can't short-circuit), inviting divergent-path bugs.

  → Routed via an error hint that teaches the engine-supported correct form
  (`OPTIONAL MATCH … WITH x, COUNT(boundVar)`).
- **MATCH-pattern label union** — **already supported** (`parser.rs:190-196`
  loops `while c.eat(&Token::Pipe)`; verified: `MATCH (n:Class|Struct|Enum)…`
  parses, returns rows). No work.

## In scope

### 1. `EXISTS { pattern }` / `NOT EXISTS (pattern)` — WHERE predicate

The core primitive. Solves the 5 `NOT EXISTS` orphan queries directly.

- **ast.rs:** `Expr::ExistsPattern { pattern: Pattern, negated: bool }` —
  reuses the existing `Pattern` struct.
- **lexer.rs:** recognize `EXISTS` keyword token.
- **parser.rs:** in `parse_primary`, add an `EXISTS` arm parsing
  `EXISTS (pattern)` and `EXISTS { pattern }`. `NOT EXISTS` falls out of the
  existing unary-`NOT` handling wrapping the `EXISTS` primary.
- **executor.rs:** `eval_expr` arm reuses `walk_rel` (CSR adjacency,
  `O(deg)`) with a **short-circuiting** emit closure — set a found-flag on the
  first matching edge, ignore the rest. MUST NOT materialize the sub-pattern
  result set. Pattern variables already bound in the outer scope (`f`) are
  fixed; unbound ones (`n`) are the traversal frontier.

**Performance:** verified against the existing `walk_rel` (executor.rs:1239) —
CSR `out_offsets`/`in_offsets` give `O(deg(node))` per existence check with
short-circuit. This is *faster* than the LLM's current `COUNT(…)=0` workaround,
which cannot short-circuit. No new complexity class; p50 must not regress.

### 2. `IS NULL` / `IS NOT NULL` — predicate

Solves `OPTIONAL MATCH (c)-[r]->(f) WHERE r IS NULL` (OPTIONAL MATCH itself is
already supported — `parser.rs:121`).

- **lexer.rs:** recognize `IS` / `NULL` keywords.
- **ast.rs:** `Expr::IsNull { expr: Box<Expr>, negated: bool }`.
- **parser.rs:** in the comparison layer, after a primary, accept
  `IS [NOT] NULL`.
- **executor.rs:** `eval_expr` arm — `eval(expr) == Value::Null` ⇒ `true`
  (XOR `negated`).

### 3. Error guidance via `CypherError::Parse.hint`

ecp already has a teaching mechanism, but it is narrow: `diagnostics.rs`
suggests near-miss *property* names (`n.file` → `n.filePath`) only.
`CypherError::Parse` (error.rs) carries `offset/expected/found` — no hint.
Reuse the existing `suggestion`-style pattern by adding an optional hint:

- **error.rs:** add `hint: Option<String>` to `CypherError::Parse`; `Display`
  appends `\n  hint: <…>` when present.
- **parser.rs:** when a parse fails on a recognizable non-subset construct,
  fill the hint:
  - `LEFT JOIN` / `RIGHT JOIN` / `INNER JOIN` → "SQL syntax; Cypher uses
    OPTIONAL MATCH + WHERE x IS NULL for left-join semantics".
  - `CALL … YIELD` → "stored procedures unsupported; for a node's edge types
    use `ecp inspect --name X`".
  - `COUNT(` immediately followed by `(` (pattern-in-aggregate) → "pattern in
    aggregate is non-standard; use `WHERE EXISTS((a)-[:R]->(b))` to test
    existence, or `OPTIONAL MATCH (a)-[:R]->(b) WITH b, COUNT(a) AS n`".

### 4. Telemetry misclassification fix

`ecp usage --failures` shows ambiguous-symbol errors landing in `other` and one
in `cypher-parse`:

- **Root cause A:** `impact` builds ambiguous-symbol failures as
  `EcpError::InvalidArgument(string)`, bypassing the typed
  `EcpError::AmbiguousSymbol` arm (`telemetry_cli.rs:49`) that would correctly
  map to `no-such-symbol`. → impact's ambiguous path should raise the typed
  variant.
- **Root cause B:** `classify_invalid_argument` / `classify_freeform`
  (`telemetry_cli.rs:65/101`) run `m.contains("parse")` over the *entire*
  message, including the candidate `filePath` list — a candidate path
  containing "parse" mis-triggers `cypher-parse`. → match against the message's
  first line (the error subject before the candidate dump), not the whole
  payload.

### 5. Telemetry version attribution

`CallRecord` (`telemetry_cli.rs:171`) records `ts` (RFC3339) but **no ecp
version**. Without it, post-fix dashboards can't attribute "cypher error rate
dropped" to a version, and old records (old classifier) can't be told apart
from new ones.

- **telemetry_cli.rs:** add `version: &str` field =
  `env!("CARGO_PKG_VERSION")`.
- **Backward compatibility:** records are jsonl read via
  `serde_json::from_str::<Value>` (`usage.rs:148/185`) — dynamic, field-by-
  field. Old records simply lack the field; the reader defaults absent
  `version` to `"unknown"`. No migration, append-only safe.

## Testing (CLAUDE.md: parser changes need 14-language coverage)

- **parser unit tests** (in `parser.rs` `#[cfg(test)]`): AST shape for
  `EXISTS`, `NOT EXISTS`, `IS NULL`, `IS NOT NULL`.
- **executor tests:** orphan query returns correct rows against the existing
  fixture graph; assert short-circuit (a found-flag, not full traversal).
- **14-language orphan test** (new `crates/ecp-cli/tests/cypher_exists_predicate.rs`):
  for each of TypeScript, JavaScript, Python, Java, Kotlin, C#, Go, Rust, PHP,
  Ruby, Swift, C, C++, Dart — a "function with no caller" query returns the
  expected non-empty set against that language's fixture.
- **error-hint tests** (extend `cypher_error_messages.rs`): `LEFT JOIN`,
  `CALL YIELD`, `COUNT(pattern)` produce the guiding hint text.
- **telemetry tests** (extend `telemetry_cli.rs` `#[cfg(test)]`): ambiguous
  message → `no-such-symbol`; candidate-path-with-"parse" → not `cypher-parse`;
  `version` field present and back-compat default.
- **TDD:** failing test first for each unit.
- **Benchmark:** subagent runs `scripts/benchmark/benchmark_ecp.py` before/after
  to confirm cypher p50 does not regress.

## File touch list

| File | Change |
|---|---|
| `crates/ecp-core/src/cypher/ast.rs` | `Expr::ExistsPattern`, `Expr::IsNull` |
| `crates/ecp-core/src/cypher/lexer.rs` | `EXISTS` / `IS` / `NULL` tokens |
| `crates/ecp-core/src/cypher/parser.rs` | EXISTS primary; `IS [NOT] NULL`; parse-fail hints |
| `crates/ecp-core/src/cypher/executor.rs` | `eval_expr` arms (short-circuit EXISTS; IS NULL) |
| `crates/ecp-core/src/cypher/error.rs` | `Parse.hint` field + Display |
| `crates/ecp-cli/src/telemetry_cli.rs` | ambiguous typed-variant fix; first-line match; `version` field |
| `crates/ecp-cli/src/commands/impact*` | raise typed `AmbiguousSymbol` |
| `crates/ecp-cli/tests/cypher_exists_predicate.rs` | new — 14-lang orphan |
| `crates/ecp-cli/tests/cypher_error_messages.rs` | extend — hints |

## Success criteria

- 4 of 5 orphan phrasings parse & execute correctly; the 5th (`COUNT(pattern)`)
  and `LEFT JOIN`/`CALL` produce an actionable hint.
- cypher p50 latency unchanged (benchmark).
- telemetry: ambiguous → `no-such-symbol`; no false `cypher-parse`; records
  carry `version`.
- 14-language orphan tests green.
