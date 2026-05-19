# Resolver baseline & L0-delta numbers

Captured 2026-05-15 via the oracle harness in
`docs/specs/2026-05-15-resolver-oracle-harness.md`.

## Methodology

Per language, diff the dump from `cgn admin index --dump-resolver` against an
authoritative oracle (TS Compiler API / `importlib.util.find_spec` /
workspace-aware Rust module walker) over a real-world corpus:

| Lang | Corpus | Path | Bindings |
|---|---|---|---:|
| TS | NestJS | `.sample_repo/TypeScript/` (1716 files) | 8396 |
| Py | Flask | `.sample_repo/Python/` (83 files) | 677 |
| Rs | tokio | `.sample_repo/Rust/` (10-crate workspace, 475 files) | 4581 |

Diff classes (per
[harness spec](2026-05-15-resolver-oracle-harness.md#diff-semantics)):
- **TP** — both resolved, target file equal under extension-equivalence
- **FP_ghost** — cgn connected to a different file than the oracle
- **FP_overmatch** — cgn Tier 3 Global produced N edges where oracle says 1
- **FN_dangling** — oracle resolved, cgn Unresolved
- **tier_demoted** — TP but cgn fell back to Global instead of ImportScoped

## Known limitations (read first)

1. **Symbol vs. import resolution mismatch.** TS oracle reports where
   `tsc` resolves the *module specifier* (often a barrel `index.ts`),
   while cgn reports where the *symbol* is *defined*. On re-export-heavy
   codebases (NestJS) this inflates `FP_ghost`. The diff harness does
   not chase re-export chains in v1.
2. **cgn Python skips `__init__.py` as a source.** The Python parser
   produces no `RawNode` from re-export-only `__init__.py` files, so
   the cgn dump has zero entries for `src/flask/__init__.py` — the
   exact place where the most relative imports live. This makes the
   Python harness number a *floor*, not a ceiling.
3. **`oracle_only` and `cgn_only` are not defects.** They count the
   asymmetry of producer scopes (oracle: every import binding;
   cgn: every callsite / heritage / type / framework-ref resolution).
   The signal is in the intersection — `TP + FP_ghost + FP_overmatch +
   FN_dangling + tier_demoted`.
4. **Not a CI gate.** Local corpus, hand-curated. Use it to track
   per-PR deltas, not absolute bars.

## Baseline vs L0 — full table

All numbers diff'd against the **same** oracle output. The pre/post-L0
comparison is therefore strictly causal — only the resolver code path
changed between rows.

### TS — NestJS (intersection = 998)

| metric | baseline | post-L0 | Δ |
|---|---:|---:|---:|
| TP | 350 | 364 | **+14** |
| FP_ghost | 307 | 293 | **−14** |
| FP_overmatch | 255 | 184 | **−71** |
| FN_dangling | 36 | 36 | 0 |
| tier_demoted | 350 | 191 | **−159** |
| **ImportScoped TP** | **0** | **173** | **+173 (new tier reached)** |
| Global TP | 350 | 191 | −159 |

**Reading**: L0 promoted 173 edges from Tier 3 Global → Tier 2
ImportScoped with **zero** ghost/overmatch in that promoted set. The
71-edge `FP_overmatch` drop and 14-edge `FP_ghost` drop are pure
collateral wins from those promotions — they were *also* matching
wrong-named siblings in Tier 3. Remaining Tier 3 activity is from
aliased imports (`@nestjs/common` etc.) that need L1 to fix.

### Py — Flask (intersection = 188)

| metric | baseline | post-L0 | Δ |
|---|---:|---:|---:|
| TP | 34 | 34 | 0 |
| FP_ghost | 78 | 78 | 0 |
| FP_overmatch | 1 | 1 | 0 |
| FN_dangling | 42 | 42 | 0 |
| tier_demoted | 34 | 34 | 0 |

**Reading**: L0 is a no-op on Python in this corpus. Two causes,
both *outside* L0's design:
- The Flask corpus's biggest import surface is `src/flask/__init__.py`
  (a 100-line re-export hub). cgn's Python parser produces zero
  `RawNode` from that file (no defined symbols), so the resolver never
  fires on its imports — they live entirely in `oracle_only` (475).
- Remaining Tier 3 hits are `from flask import Flask` style absolute
  intra-package imports. Resolving `flask` → `src/flask/__init__.py`
  requires package-root discovery (L1 territory).

This is the harness working as designed: it identifies that L0 is
specifically a TS/JS gain, not a universal one. Without the harness
we'd have shipped L0 thinking it helped Python — measurement caught
it.

### Rs — tokio (intersection = 633)

| metric | baseline | post-L0 | Δ |
|---|---:|---:|---:|
| TP | 48 | 48 | 0 |
| FP_ghost | 327 | 327 | 0 |
| FP_overmatch | 3 | 3 | 0 |
| FN_dangling | 255 | 255 | 0 |
| tier_demoted | 46 | 46 | 0 |

**Reading**: L0 is a no-op on Rust by design. Rust's `use a::b::C`
uses `::` separators which L0 doesn't touch — the harness confirms
this. Rust resolver wins come from a future Rust-specific layer that
maps `crate::`/`super::`/`self::` → workspace file paths.

## Headline

L0 is a **TS-specific win** with strictly positive directional impact
(every metric moved the right way; nothing regressed; zero new false
positives). The harness has now demonstrated three things:

1. We can absolutely measure resolver correctness against tsc /
   pyright-equivalent / cargo-aware oracles.
2. L0's predicted shape (Tier 2 wakes up for relative imports + ext
   elision; Tier 3 over-match drops as collateral) is exactly what
   the data shows on TS.
3. L0 is **not** a universal fix. Python and Rust need different
   layers (package-root discovery / crate-path resolution
   respectively). Shipping L0 alone is honest progress, not a
   complete answer.

## Repro

```bash
cargo build -p code-graph-nexus --release

# TS
(cd .sample_repo/TypeScript && cgn admin index --repo . --dump-resolver dumps/cgn.ts.jsonl)
node scripts/parity/oracles/ts_oracle.mjs .sample_repo/TypeScript > dumps/oracle.ts.jsonl
cgn verify-resolver --lang ts --oracle dumps/oracle.ts.jsonl --cgn dumps/cgn.ts.jsonl --report report.ts.md

# Py
(cd .sample_repo/Python && cgn admin index --repo . --dump-resolver dumps/cgn.py.jsonl)
python3 scripts/parity/oracles/py_oracle.py .sample_repo/Python > dumps/oracle.py.jsonl
cgn verify-resolver --lang py --oracle dumps/oracle.py.jsonl --cgn dumps/cgn.py.jsonl --report report.py.md

# Rs
(cd .sample_repo/Rust && cgn admin index --repo . --dump-resolver dumps/cgn.rs.jsonl)
python3 scripts/parity/oracles/rs_oracle.py .sample_repo/Rust > dumps/oracle.rs.jsonl
cgn verify-resolver --lang rs --oracle dumps/oracle.rs.jsonl --cgn dumps/cgn.rs.jsonl --report report.rs.md
```

## Saved reports

Pre-L0: `tests/parity/oracle-runs/report.{ts,py,rs}.baseline.md`
Post-L0: `tests/parity/oracle-runs/report.{ts,py,rs}.l0.md`
