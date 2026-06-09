# Cypher EXISTS/IS NULL predicates + error hints + telemetry attribution — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the LLM's intuitive orphan-finding Cypher (`NOT EXISTS(pattern)`, `OPTIONAL MATCH … IS NULL`) execute correctly, guide the unsupported phrasings with hints, and make the telemetry failure log correctly-classified and version-attributable.

**Architecture:** Extend the hand-written Cypher subset engine (`crates/ecp-core/src/cypher/`) with two new WHERE predicates — `EXISTS`/`NOT EXISTS` (short-circuiting over the existing CSR adjacency via `walk_rel`) and `IS [NOT] NULL`. Add a `hint` field to parse errors for non-subset constructs. Separately fix two telemetry-classification bugs and add a `version` field to call records.

**Tech Stack:** Rust, hand-written lexer/parser/executor, `rkyv` zero-copy graph, `serde_json` jsonl telemetry.

**Spec:** `docs/superpowers/specs/2026-06-08-cypher-exists-predicate-design.md`

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/ecp-core/src/cypher/lexer.rs` | tokenize | add `Is`, `Exists` keyword tokens |
| `crates/ecp-core/src/cypher/ast.rs` | AST types | add `Expr::ExistsPattern`, `Expr::IsNull` |
| `crates/ecp-core/src/cypher/parser.rs` | parse | EXISTS primary; `IS [NOT] NULL` postfix; parse-fail hints |
| `crates/ecp-core/src/cypher/executor.rs` | evaluate | `eval_expr` arms for the two new exprs (EXISTS short-circuits) |
| `crates/ecp-core/src/cypher/error.rs` | error type | add `hint: Option<String>` to `Parse` |
| `crates/ecp-core/src/error.rs` | core error | add `candidates` to `AmbiguousSymbol` |
| `crates/ecp-cli/src/commands/impact.rs` | impact cmd | raise typed `AmbiguousSymbol` with candidates |
| `crates/ecp-cli/src/telemetry_cli.rs` | telemetry | first-line classify; `version` field |
| `crates/ecp-cli/tests/cypher_exists_predicate.rs` | test | NEW — 14-language orphan queries |
| `crates/ecp-cli/tests/cypher_error_messages.rs` | test | extend — hint assertions |

---

## Task 1: Lexer — `IS` and `EXISTS` keyword tokens

**Files:**
- Modify: `crates/ecp-core/src/cypher/lexer.rs:4` (Token enum), `:320` (keyword match)
- Test: `crates/ecp-core/src/cypher/lexer.rs` `#[cfg(test)]`

- [ ] **Step 1: Write failing tests**

Add to the `tests` mod in `lexer.rs`:

```rust
#[test]
fn keyword_is_and_exists() {
    assert_eq!(lex("IS"), vec![Token::Is]);
    assert_eq!(lex("is"), vec![Token::Is]);
    assert_eq!(lex("EXISTS"), vec![Token::Exists]);
    assert_eq!(lex("exists"), vec![Token::Exists]);
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p ecp-core cypher::lexer::tests::keyword_is_and_exists`
Expected: FAIL — `no variant Token::Is`.

- [ ] **Step 3: Add the tokens**

In the `Token` enum (after `In,` at line 23):

```rust
    Is,
    Exists,
```

In the keyword match (after `"IN" => Token::In,` at line 344):

```rust
                "IS" => Token::Is,
                "EXISTS" => Token::Exists,
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p ecp-core cypher::lexer::tests::keyword_is_and_exists`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-core/src/cypher/lexer.rs
git commit -m "feat(cypher): lex IS and EXISTS keywords"
```

---

## Task 2: AST — `ExistsPattern` and `IsNull` variants

**Files:**
- Modify: `crates/ecp-core/src/cypher/ast.rs:78` (Expr enum)

- [ ] **Step 1: Add the variants**

In `pub enum Expr` (after the `HasLabel(String, Vec<String>)` variant, before `FunCall`):

```rust
    /// `EXISTS { pattern }` / `NOT EXISTS (pattern)` WHERE predicate.
    /// `negated` folds a leading NOT so the executor short-circuits on the
    /// first matching edge without an extra UnaryOp wrap. Outer-scope-bound
    /// vars in the pattern are fixed; unbound vars form the traversal frontier.
    ExistsPattern {
        pattern: Pattern,
        negated: bool,
    },
    /// `x IS [NOT] NULL`. Distinct from `BinOp(Eq, x, NULL)` because NULL
    /// comparison in Cypher is itself NULL (never true), so `= NULL` can't
    /// express the test the way `IS NULL` does.
    IsNull {
        expr: Box<Expr>,
        negated: bool,
    },
```

This references `Pattern` (defined at `ast.rs:23`) — already in scope.

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p ecp-core 2>&1 | grep -E "error|warning: unused" | head`
Expected: non-exhaustive-match errors in `executor.rs`/`parser.rs` (those arms come in Tasks 3–5). No *syntax* errors in `ast.rs`. This is expected mid-plan; do NOT add executor arms yet.

- [ ] **Step 3: Commit**

```bash
git add crates/ecp-core/src/cypher/ast.rs
git commit -m "feat(cypher): add ExistsPattern and IsNull AST variants"
```

---

## Task 3: Executor — evaluate `IsNull` (simpler, no traversal)

Do `IsNull` before `ExistsPattern` — it's a pure scalar check and unblocks compilation incrementally.

**Files:**
- Modify: `crates/ecp-core/src/cypher/executor.rs` (`eval_expr`, near the `HasLabel` arm ~line 1399)
- Test: `crates/ecp-core/src/cypher/executor.rs` `#[cfg(test)]`

- [ ] **Step 1: Write failing test**

Add to the executor `tests` mod (reuse an existing fixture builder; the
2-node graph at the `OPTIONAL MATCH` test ~line 2396 is the model):

```rust
#[test]
fn is_null_on_unbound_optional_edge() {
    // lone node has no outgoing edge; OPTIONAL MATCH binds b=null,
    // WHERE b IS NULL must keep the row.
    let g = build_lone_node_graph(); // existing helper used by OPTIONAL tests
    let rows = run_query(&g, "MATCH (a) OPTIONAL MATCH (a)-->(b) WHERE b IS NULL RETURN a.name");
    assert_eq!(rows.len(), 1);
}
```

(If `build_lone_node_graph`/`run_query` differ in name, use the exact helpers the neighbouring OPTIONAL MATCH test uses — grep `fn ` in the test mod.)

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p ecp-core cypher::executor::tests::is_null_on_unbound_optional_edge`
Expected: FAIL — non-exhaustive match or parse error (parser arm lands in Task 5; this test will pass only after Task 5. To verify the *executor* arm in isolation now, assert via a hand-built AST instead — see Step 3 note).

- [ ] **Step 3: Add the `eval_expr` arm**

In `eval_expr`'s match, after the `HasLabel` arm (~line 1406), before `FunCall`:

```rust
        IsNull { expr, negated } => {
            let v = eval_expr(expr, b, graph, cache)?;
            let is_null = matches!(v, Value::Null);
            Ok(Value::Bool(is_null ^ negated))
        }
```

Note: until Task 5 wires the parser, assert this arm with a hand-constructed
`Expr::IsNull { expr: Box::new(Expr::Var("b".into())), negated: false }` in the
test so it's verifiable independent of parsing. Convert to the string-query
form once Task 5 lands.

- [ ] **Step 4: Run, verify pass (AST-level)**

Run: `cargo test -p ecp-core cypher::executor::tests::is_null`
Expected: PASS for the AST-level assertion.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-core/src/cypher/executor.rs
git commit -m "feat(cypher): evaluate IS [NOT] NULL predicate"
```

---

## Task 4: Executor — evaluate `ExistsPattern` (short-circuit traversal)

**Files:**
- Modify: `crates/ecp-core/src/cypher/executor.rs` (`eval_expr`, after the `IsNull` arm)
- Test: `crates/ecp-core/src/cypher/executor.rs` `#[cfg(test)]`

- [ ] **Step 1: Write failing test (AST-level, short-circuit observable)**

```rust
#[test]
fn exists_pattern_short_circuits_on_incoming_edge() {
    // caller -> callee (Calls). callee has an incoming Calls edge; lone does not.
    let g = build_caller_callee_graph(); // existing helper ~line 2485 region
    // NOT EXISTS((n)-[:Calls]->(callee))  ⇒  false for callee, true for caller/lone
    let rows = run_query(
        &g,
        "MATCH (f:Function) WHERE NOT EXISTS((n)-[:Calls]->(f)) RETURN f.name",
    );
    assert!(rows.iter().all(|r| r != "callee"));
    assert!(rows.iter().any(|r| r == "caller"));
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p ecp-core cypher::executor::tests::exists_pattern_short_circuits_on_incoming_edge`
Expected: FAIL (parser arm not yet present → test via hand-built AST first, same note as Task 3; or expect FAIL until Task 5).

- [ ] **Step 3: Add the `eval_expr` arm**

After the `IsNull` arm. The pattern is one node–rel–node hop (the orphan
shape); evaluate existence by fixing any outer-bound endpoint and walking via
`walk_rel` with a short-circuiting closure:

```rust
        ExistsPattern { pattern, negated } => {
            // Orphan-shape predicate: a single rel between two node pats, at
            // least one endpoint bound in the outer scope. Walk from the bound
            // endpoint over the rel type; stop at the first match (existence,
            // not count). Falls back to false if neither endpoint is bound.
            let found = pattern_exists(pattern, b, graph);
            Ok(Value::Bool(found ^ negated))
        }
```

Add the helper near `walk_rel` (~line 1239):

```rust
/// Existence check for an EXISTS predicate's single-hop pattern. Returns true
/// as soon as one edge satisfies it — no full traversal, no result set.
/// Determines the anchor (the endpoint already bound in `b`) and walks `rel`
/// from it, checking the other endpoint's kind constraints.
fn pattern_exists(pattern: &Pattern, b: &Binding, graph: &ArchivedZeroCopyGraph) -> bool {
    // pattern.start is the first NodePat; pattern.rels carries (RelPat, NodePat).
    let Some((rel, other)) = pattern.rels.first() else {
        // Bare node pattern EXISTS((n)) — true iff the node var is bound.
        return pattern_start_var(pattern).is_some_and(|v| b.node_vars.contains_key(v));
    };
    // Find which side is bound: prefer the start, else the other endpoint.
    let (anchor_idx, anchor_is_start) = match resolve_anchor(pattern, other, b) {
        Some(x) => x,
        None => return false, // neither endpoint bound — can't anchor
    };
    // Effective direction: if anchored on the start, walk rel.dir as-is;
    // if anchored on the far endpoint, invert.
    let dir = if anchor_is_start { rel.dir } else { invert_dir(rel.dir) };
    let probe_rel = RelPat { dir, ..rel.clone() };
    let target_pat = if anchor_is_start { other } else { pattern_start_node(pattern) };
    let mut hit = false;
    walk_rel(anchor_idx, &probe_rel, graph, |tgt, _edge| {
        if hit {
            return; // already satisfied — ignore remaining (short-circuit)
        }
        if node_matches(&graph.nodes[tgt as usize], tgt, target_pat, graph) {
            hit = true;
        }
    });
    hit
}
```

Implement the three tiny helpers (`pattern_start_var`, `resolve_anchor`,
`invert_dir`, `pattern_start_node`) inline against the actual `Pattern` shape.
Check `ast.rs:23` for the exact field names (`start` / `rels` / whatever the
struct uses) and adapt — do NOT guess; read the struct first.

- [ ] **Step 4: Run, verify pass (AST-level)**

Run: `cargo test -p ecp-core cypher::executor::tests::exists_pattern`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-core/src/cypher/executor.rs
git commit -m "feat(cypher): evaluate EXISTS pattern predicate with short-circuit"
```

---

## Task 5: Parser — EXISTS primary + `IS [NOT] NULL` postfix

**Files:**
- Modify: `crates/ecp-core/src/cypher/parser.rs` (`parse_primary` ~line 536; `parse_comparison` postfix ~line 462)
- Test: `crates/ecp-core/src/cypher/parser.rs` `#[cfg(test)]`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn parse_exists_pattern() {
    match ex("EXISTS((n)-[:Calls]->(f))") {
        Expr::ExistsPattern { negated, .. } => assert!(!negated),
        other => panic!("expected ExistsPattern, got {other:?}"),
    }
}

#[test]
fn parse_not_exists_pattern() {
    // NOT EXISTS(...) — the leading NOT is folded into negated:true.
    let toks = tokenize("WHERE NOT EXISTS((n)-[:Calls]->(f))").unwrap();
    let e = parse_where(&mut Cursor::new(&toks)).unwrap();
    match e {
        Expr::ExistsPattern { negated, .. } => assert!(negated),
        other => panic!("expected folded ExistsPattern, got {other:?}"),
    }
}

#[test]
fn parse_is_null_and_is_not_null() {
    match ex("r IS NULL") {
        Expr::IsNull { negated, .. } => assert!(!negated),
        other => panic!("expected IsNull, got {other:?}"),
    }
    match ex("r IS NOT NULL") {
        Expr::IsNull { negated, .. } => assert!(negated),
        other => panic!("expected IsNull negated, got {other:?}"),
    }
}
```

(`ex` is the existing test helper that parses one expression — confirm its name in the test mod.)

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p ecp-core cypher::parser::tests::parse_exists_pattern`
Expected: FAIL — parser doesn't handle EXISTS.

- [ ] **Step 3a: EXISTS primary**

In `parse_primary` (line 536), as the FIRST check — BEFORE `if c.eat(&Token::LParen)` (which would otherwise swallow `EXISTS`'s `(`):

```rust
    if c.eat(&Token::Exists) {
        // EXISTS (pattern)  or  EXISTS { pattern }
        let brace = c.eat(&Token::LBrace);
        if !brace {
            c.expect(&Token::LParen)?;
        }
        let pattern = parse_pattern(c)?;
        if brace {
            c.expect(&Token::RBrace)?;
        } else {
            c.expect(&Token::RParen)?;
        }
        return Ok(Expr::ExistsPattern { pattern, negated: false });
    }
```

- [ ] **Step 3b: Fold `NOT EXISTS` in `parse_not`**

`parse_not` (line 450) currently wraps any NOT in `UnaryOp::Not`. Special-case
a directly-following EXISTS so the executor sees `negated:true` (cleaner short-
circuit than evaluating the inner then negating):

```rust
fn parse_not(c: &mut Cursor) -> Result<Expr, CypherError> {
    if c.eat(&Token::Not) {
        // NOT EXISTS(...) folds into ExistsPattern{negated:true}.
        if c.check(&Token::Exists) {
            let inner = parse_comparison(c)?;
            if let Expr::ExistsPattern { pattern, .. } = inner {
                return Ok(Expr::ExistsPattern { pattern, negated: true });
            }
            return Ok(Expr::UnaryOp(UnaryOp::Not, Box::new(inner)));
        }
        let inner = parse_not(c)?;
        Ok(Expr::UnaryOp(UnaryOp::Not, Box::new(inner)))
    } else {
        parse_comparison(c)
    }
}
```

- [ ] **Step 3c: `IS [NOT] NULL` postfix**

In `parse_comparison`, in the postfix-operator block (after the `Contains`
check ~line 509, before the infix comparisons at line 511):

```rust
    if c.eat(&Token::Is) {
        let negated = c.eat(&Token::Not);
        c.expect(&Token::Null)?;
        return Ok(Expr::IsNull { expr: Box::new(lhs), negated });
    }
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p ecp-core cypher::parser::tests::parse_exists_pattern cypher::parser::tests::parse_not_exists_pattern cypher::parser::tests::parse_is_null_and_is_not_null`
Expected: PASS. Then convert the Task 3/4 executor tests from hand-built AST to string-query form and re-run them — now green end-to-end.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-core/src/cypher/parser.rs crates/ecp-core/src/cypher/executor.rs
git commit -m "feat(cypher): parse EXISTS pattern and IS [NOT] NULL predicates"
```

---

## Task 6: 14-language orphan integration test

**Files:**
- Create: `crates/ecp-cli/tests/cypher_exists_predicate.rs`

- [ ] **Step 1: Find the existing per-language fixture harness**

Run: `ls crates/ecp-analyzer/tests/ | grep -E "frameworks|named" | head` and read one (e.g. `go_frameworks.rs`) to copy the fixture-build + query pattern used across the suite. Reuse whatever helper builds a graph from inline source per language. Do NOT invent a new harness.

- [ ] **Step 2: Write the test (one assertion per language)**

For each of the 14 languages, build a 2-function fixture where `caller` calls
`callee`, then assert the orphan query returns `caller` (no incoming Calls) and
not `callee`:

```rust
// crates/ecp-cli/tests/cypher_exists_predicate.rs
// For each language: source with `caller` calling `callee`.
// Query: MATCH (f:Function) WHERE NOT EXISTS((n)-[:Calls]->(f)) RETURN f.name
// Assert: contains "caller", excludes "callee".

const CASES: &[(&str, &str, &str)] = &[
    ("typescript", "ts", "function callee(){}\nfunction caller(){callee();}"),
    ("javascript", "js", "function callee(){}\nfunction caller(){callee();}"),
    ("python", "py", "def callee():\n    pass\ndef caller():\n    callee()"),
    ("java", "java", "class A{ void callee(){} void caller(){callee();} }"),
    ("kotlin", "kt", "fun callee(){}\nfun caller(){callee()}"),
    ("csharp", "cs", "class A{ void Callee(){} void Caller(){Callee();} }"),
    ("go", "go", "package m\nfunc callee(){}\nfunc caller(){callee()}"),
    ("rust", "rs", "fn callee(){}\nfn caller(){callee();}"),
    ("php", "php", "<?php function callee(){} function caller(){callee();}"),
    ("ruby", "rb", "def callee; end\ndef caller; callee; end"),
    ("swift", "swift", "func callee(){}\nfunc caller(){callee()}"),
    ("c", "c", "void callee(){}\nvoid caller(){callee();}"),
    ("cpp", "cpp", "void callee(){}\nvoid caller(){callee();}"),
    ("dart", "dart", "void callee(){}\nvoid caller(){callee();}"),
];

#[test]
fn orphan_query_finds_uncalled_function_across_14_languages() {
    for (lang, ext, src) in CASES {
        let names = run_orphan_query(lang, ext, src); // build graph + run cypher
        assert!(names.contains(&"caller".to_string()), "{lang}: caller should be orphan");
        assert!(!names.contains(&"callee".to_string()), "{lang}: callee has a caller");
    }
}
```

Implement `run_orphan_query` using the harness found in Step 1 (build a graph
from the source, run the cypher query through the same entry point the CLI
uses). Method/identifier names (Java/C# use methods, not free functions) —
adjust the query to `MATCH (f) WHERE NOT EXISTS((n)-[:Calls]->(f))` without the
`:Function` kind filter if a language emits Method, OR run per-language kind.

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p egent-code-plexus --test cypher_exists_predicate`
Expected: PASS for all 14. If a language emits `Method` not `Function`, fix the per-language query/kind — a failure here is a real cross-language gap to surface, not a test to weaken.

- [ ] **Step 4: Commit**

```bash
git add crates/ecp-cli/tests/cypher_exists_predicate.rs
git commit -m "test(cypher): 14-language orphan query via NOT EXISTS"
```

---

## Task 7: Parse-error hints for non-subset constructs

**Files:**
- Modify: `crates/ecp-core/src/cypher/error.rs:18` region (`Parse` variant + Display)
- Modify: `crates/ecp-core/src/cypher/parser.rs` (fill hint at the relevant parse-fail sites)
- Test: extend `crates/ecp-cli/tests/cypher_error_messages.rs`

- [ ] **Step 1: Add `hint` to `CypherError::Parse`**

In `crates/ecp-core/src/cypher/error.rs`:

```rust
    Parse {
        offset: usize,
        expected: String,
        found: String,
        hint: Option<String>,
    },
```

Update the `Display` arm:

```rust
            Self::Parse { offset, expected, found, hint } => {
                write!(f, "parse error at byte {offset}: expected {expected}, found {found}")?;
                if let Some(h) = hint {
                    write!(f, "\n  hint: {h}")?;
                }
                Ok(())
            }
```

Update every existing `CypherError::Parse { … }` construction and the
`Cursor::err` helper to pass `hint: None`. Grep: `cargo build -p ecp-core 2>&1 | grep "missing field"` then fix each site.

- [ ] **Step 2: Write failing tests**

In `crates/ecp-cli/tests/cypher_error_messages.rs`:

```rust
#[test]
fn left_join_gets_sql_hint() {
    let err = run_cypher_expect_err("MATCH (f) WITH f LEFT JOIN (x)-[r]->(f) RETURN f");
    assert!(err.contains("hint:"), "got: {err}");
    assert!(err.contains("OPTIONAL MATCH"), "got: {err}");
}

#[test]
fn call_yield_gets_procedure_hint() {
    let err = run_cypher_expect_err("MATCH (n) CALL ecp.edge_types(n) YIELD relation RETURN n");
    assert!(err.contains("ecp inspect"), "got: {err}");
}

#[test]
fn count_pattern_gets_exists_hint() {
    let err = run_cypher_expect_err("MATCH (f) WITH f, COUNT((x)-[r]->(f)) AS n RETURN f");
    assert!(err.contains("EXISTS") || err.contains("OPTIONAL MATCH"), "got: {err}");
}
```

(`run_cypher_expect_err` — reuse the existing error-message test harness in that file; confirm its name.)

- [ ] **Step 3: Run, verify fail**

Run: `cargo test -p egent-code-plexus --test cypher_error_messages left_join_gets_sql_hint`
Expected: FAIL — no hint text.

- [ ] **Step 4: Emit hints at parse-fail sites**

The cleanest single chokepoint: when `parse_clause`/top-level parse encounters
an unexpected keyword token after a valid prefix, classify it. Add a helper in
`parser.rs`:

```rust
/// Maps a recognizable non-subset construct (by the unexpected token sequence)
/// to a guiding hint. Returns None for ordinary parse errors.
fn hint_for_unsupported(tokens: &[Token], pos: usize) -> Option<String> {
    // SQL JOIN: an Ident("LEFT"|"RIGHT"|"INNER") followed by Ident("JOIN").
    if let (Some(Token::Ident(a)), Some(Token::Ident(b))) = (tokens.get(pos), tokens.get(pos + 1)) {
        let a = a.to_ascii_uppercase();
        if matches!(a.as_str(), "LEFT" | "RIGHT" | "INNER" | "FULL") && b.eq_ignore_ascii_case("join") {
            return Some("SQL syntax detected; Cypher uses OPTIONAL MATCH (a)-[r]->(b) WHERE r IS NULL for left-join semantics".into());
        }
        if a == "CALL" {
            return Some("stored procedures (CALL … YIELD) are unsupported; for a node's edge types use `ecp inspect --name X`".into());
        }
    }
    None
}
```

For `COUNT(` immediately followed by `(` (pattern-in-aggregate), detect at the
aggregate-arg parse site and attach:

```rust
// where COUNT's arg parse fails on a `(` opening a pattern:
hint: Some("pattern in aggregate is non-standard; use WHERE EXISTS((a)-[:R]->(b)) to test existence, or OPTIONAL MATCH (a)-[:R]->(b) WITH b, COUNT(a) AS n".into()),
```

Wire `hint_for_unsupported` into the top-level parse-error return so the
unexpected-keyword path carries the hint. Keep ordinary errors `hint: None`.

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p egent-code-plexus --test cypher_error_messages`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ecp-core/src/cypher/error.rs crates/ecp-core/src/cypher/parser.rs crates/ecp-cli/tests/cypher_error_messages.rs
git commit -m "feat(cypher): hint LEFT JOIN/CALL/COUNT-pattern toward supported forms"
```

---

## Task 8: Telemetry — typed ambiguous + candidates field

**Files:**
- Modify: `crates/ecp-core/src/error.rs:19` (`AmbiguousSymbol`)
- Modify: `crates/ecp-cli/src/commands/impact.rs:961`
- Test: `crates/ecp-cli/src/telemetry_cli.rs` `#[cfg(test)]`

- [ ] **Step 1: Write failing test**

In `telemetry_cli.rs` tests (model on the existing `classify_structured_variants` ~line 231):

```rust
#[test]
fn ambiguous_with_candidates_classifies_as_no_such_symbol() {
    let e = EcpError::AmbiguousSymbol {
        name: "check".into(),
        count: 8,
        candidates: Some("a.rs,Function,9\nb.rs,Function,20".into()),
    };
    assert_eq!(classify_error(&e), "no-such-symbol");
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p egent-code-plexus telemetry_cli::tests::ambiguous_with_candidates`
Expected: FAIL — `AmbiguousSymbol` has no `candidates` field.

- [ ] **Step 3: Add the field + Display**

In `crates/ecp-core/src/error.rs`:

```rust
    #[error("symbol name '{name}' is ambiguous ({count} candidates){}", candidates.as_ref().map(|c| format!(" — add --file or --kind:\ncandidates[filePath,kind,line]:\n{c}")).unwrap_or_else(|| " — pass --uid".into()))]
    AmbiguousSymbol { name: String, count: usize, candidates: Option<String> },
```

Update the existing `telemetry_cli.rs:239` test constructor and any other
`AmbiguousSymbol { name, count }` site to add `candidates: None`. Grep via
`cargo build 2>&1 | grep "missing field"`.

- [ ] **Step 4: impact raises the typed variant**

In `impact.rs:961`, replace the `InvalidArgument(format!(...))` with:

```rust
        return Err(EcpError::AmbiguousSymbol {
            name: fqn_label.to_string(),
            count: matches.len(),
            candidates: Some(candidate_lines),
        });
```

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p egent-code-plexus telemetry_cli::tests`
Expected: PASS. Also `cargo build -p egent-code-plexus` clean.

- [ ] **Step 6: Commit**

```bash
git add crates/ecp-core/src/error.rs crates/ecp-cli/src/commands/impact.rs crates/ecp-cli/src/telemetry_cli.rs
git commit -m "fix(telemetry): impact ambiguous-symbol uses typed variant, keeps candidate list"
```

---

## Task 9: Telemetry — first-line classification (kill false cypher-parse)

**Files:**
- Modify: `crates/ecp-cli/src/telemetry_cli.rs:63` (`classify_invalid_argument`), `:99` (`classify_freeform`)
- Test: `crates/ecp-cli/src/telemetry_cli.rs` `#[cfg(test)]`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn candidate_path_containing_parse_is_not_cypher_parse() {
    // A candidate filePath with "parse" in it must not trigger cypher-parse.
    let msg = "'check' is ambiguous (2 candidates) — add --file or --kind\ncandidates[filePath,kind,line]:\ncrates/x/parser.rs,Function,9";
    assert_ne!(classify_invalid_argument(msg), "cypher-parse");
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p egent-code-plexus telemetry_cli::tests::candidate_path_containing_parse`
Expected: FAIL — `contains("parse")` over the whole message hits `parser.rs`.

- [ ] **Step 3: Match only the first line**

In both `classify_invalid_argument` and `classify_freeform`, change the opening
so keyword matching runs on the first line (the error subject), not the
candidate dump:

```rust
fn classify_invalid_argument(msg: &str) -> &'static str {
    // Match the error SUBJECT only — the first line. Candidate dumps and
    // multi-line payloads (filePaths containing "parse"/"label") must not
    // sway classification.
    let subject = msg.lines().next().unwrap_or(msg);
    let m = subject.to_ascii_lowercase();
    // … rest unchanged, operating on `m` …
```

Apply the identical `subject`/`m` change to `classify_freeform`.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p egent-code-plexus telemetry_cli::tests`
Expected: PASS (new test + all existing).

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-cli/src/telemetry_cli.rs
git commit -m "fix(telemetry): classify on error subject line, not candidate dump"
```

---

## Task 10: Telemetry — `version` field on CallRecord

**Files:**
- Modify: `crates/ecp-cli/src/telemetry_cli.rs` (`CallRecord` struct + `record()` ~line 171)
- Modify: `crates/ecp-cli/src/commands/usage.rs:148/185` (read default)
- Test: `crates/ecp-cli/src/telemetry_cli.rs` `#[cfg(test)]`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn call_record_includes_version() {
    let json = serde_json::to_string(&CallRecord {
        ts: "2026-06-08T00:00:00Z",
        tool: "cypher",
        duration_ms: 1,
        ok: true,
        source: "cli",
        error_kind: None,
        subcommand: None,
        error_msg: None,
        version: env!("CARGO_PKG_VERSION"),
    }).unwrap();
    assert!(json.contains("\"version\""), "got: {json}");
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p egent-code-plexus telemetry_cli::tests::call_record_includes_version`
Expected: FAIL — `CallRecord` has no `version`.

- [ ] **Step 3: Add the field**

In the `CallRecord` struct, add (mirror the existing borrowed-str fields):

```rust
    version: &'a str,
```

In `record()` (~line 171), in the struct literal:

```rust
        version: env!("CARGO_PKG_VERSION"),
```

- [ ] **Step 4: Reader default for old records**

In `usage.rs` where records are read (`from_str::<Value>` ~line 148/185), where
fields are pulled, default absent `version`:

```rust
let version = v.get("version").and_then(|x| x.as_str()).unwrap_or("unknown");
```

(Only add this if usage.rs surfaces version; if it doesn't display version yet,
the write side is enough for now and the read side stays forward-compatible by
construction. Confirm whether usage.rs needs to show it — if not, skip the
usage.rs edit and note that the field is recorded for future dashboards.)

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p egent-code-plexus telemetry_cli::tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ecp-cli/src/telemetry_cli.rs crates/ecp-cli/src/commands/usage.rs
git commit -m "feat(telemetry): record ecp version on each call for attribution"
```

---

## Task 11: Lint, full test, benchmark, real-CLI smoke

**Files:** none (verification)

- [ ] **Step 1: Clippy clean**

Run: `cargo clippy -p ecp-core -p egent-code-plexus --tests 2>&1 | grep -E "^warning|^error" | head`
Expected: no warnings/errors. Fix any. (Note: on Rust there is no py3.14 `except` quirk — ignore that CLAUDE.md rule here.)

- [ ] **Step 2: Full test suite**

Run: `cargo test -p ecp-core -p egent-code-plexus --tests 2>&1 | tail -20`
Expected: all green, including `cypher_exists_predicate` (14 langs) and telemetry tests.

- [ ] **Step 3: Real-CLI smoke (the original failing queries now work)**

```bash
cargo build -p egent-code-plexus --bin ecp --release
TARGET=$(pwd)
./target/release/ecp cypher 'MATCH (f:Function) WHERE NOT EXISTS((n)-[:Calls]->(f)) RETURN f.name LIMIT 3' --repo "$TARGET"
./target/release/ecp cypher 'MATCH (f:Function) OPTIONAL MATCH (c)-[r:Calls]->(f) WHERE r IS NULL RETURN f.name LIMIT 3' --repo "$TARGET"
./target/release/ecp cypher 'MATCH (f) WITH f LEFT JOIN (x)-[r]->(f) RETURN f' --repo "$TARGET" 2>&1 | grep -i hint
```
Expected: first two return rows (no parse error); third shows the SQL hint.

- [ ] **Step 4: Benchmark — confirm cypher p50 doesn't regress (subagent)**

Dispatch a Haiku subagent: "Run `python scripts/benchmark/benchmark_ecp.py` in `<worktree>`, report cypher p50/p99 before (origin/main build) vs after (this branch build). Confirm cypher p50 is within noise (±5ms). Report numbers only, no narrative. Do NOT add a Co-Authored-By trailer to anything."
Expected: cypher p50 unchanged within noise.

- [ ] **Step 5: Final commit if any clippy fixes**

```bash
git add -A && git commit -m "chore(cypher): clippy + verification fixups" || echo "nothing to fix"
```

---

## Self-review notes (spec coverage)

- Spec §1 EXISTS → Tasks 1,2,4,5,6. §2 IS NULL → Tasks 1,2,3,5. §3 hints → Task 7. §4 misclassification (both root causes) → Tasks 8 (typed ambiguous) + 9 (first-line). §5 version → Task 10. Non-goal pattern-in-aggregate → Task 7 hint (not implemented). 14-lang test → Task 6. Benchmark → Task 11.4. All spec requirements have a task.
- Type consistency: `Expr::ExistsPattern{pattern,negated}` / `Expr::IsNull{expr,negated}` used identically in ast/parser/executor. `AmbiguousSymbol{name,count,candidates}` consistent across error.rs/impact.rs/telemetry tests. `CypherError::Parse{…,hint}` consistent.
- Known adapt-on-read point: Task 4 `pattern_exists` helpers depend on the real `Pattern` struct field names — the plan explicitly instructs reading `ast.rs:23` first rather than guessing.
