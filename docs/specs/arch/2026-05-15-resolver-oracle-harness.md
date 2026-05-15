# Resolver Oracle Harness — JSONL Contract & verify-resolver CLI

Status: draft (2026-05-15)
Owner: enor
Branch: feat/resolver-verify-and-l0

## Why

The resolver in `crates/graph-nexus-analyzer/src/resolution/resolver.rs` has three tiers
(`SameFile` 1.0 → `ImportScoped` 0.95 → `Global` 0.7). The middle tier
fails on TS path aliases (`@/x`) and extension elision (`./foo` → `./foo.ts`),
which forces traffic into `Global` where N same-named candidates all get
0.7-confidence edges (ghost edges). We have no absolute measurement of how
bad this is.

This harness gives us absolute, repeatable numbers by diffing the resolver's
decisions against per-language official module-resolution oracles (tsc /
pyright / rustc).

It is **not a CI gate** — it depends on the local `~/.gnx` corpus and is
developer-run benchmarking infrastructure.

## JSONL Contract (gnx dump + all 3 oracle adapters)

Every line is one resolution decision. Same shape across producers so the
diff is a join on `(src_file, name)`.

```jsonc
{
  "src_file": "src/foo.ts",          // string, repo-relative POSIX path
  "name": "Button",                  // string, identifier being resolved
  "specifier": "@/components/Button",// string|null, import.source if tier 2+, else null
  "tier": "SameFile",                // "SameFile" | "ImportScoped" | "Global" | "Unresolved"
  "target_file": "src/components/Button.tsx", // string|null, repo-relative POSIX path
  "target_kind": "Function",         // string|null, NodeKind name (gnx only)
  "alt_count": 0,                    // int, number of *additional* candidates beyond target_file (Global tier)
  "confidence": 0.95                 // float|null, 0..1 — gnx writes this, oracles always 1.0 for "resolved"
}
```

Producer-specific rules:

| Field | gnx dump | TS oracle | Py oracle | Rust oracle |
|---|---|---|---|---|
| `tier` | as resolver decided | `ImportScoped` if alias/path resolved, else `Unresolved` | same | same |
| `specifier` | `import.source` if applicable | always set (the import specifier) | always set | always set |
| `target_file` | resolver output, normalized to repo-relative POSIX | tsc's resolved file, normalized | pyright/find_spec output | rustc resolved file |
| `target_kind` | gnx-only | null | null | null |
| `confidence` | tier base score | 1.0 if resolved else null | same | same |
| `alt_count` | gnx-only (oracles are deterministic) | 0 | 0 | 0 |

### Path normalization (applies to all producers)

1. Always **repo-relative**. Strip the corpus root prefix.
2. POSIX separators (`/`).
3. Do **not** strip extensions in the contract — diff harness handles
   extension-equivalence (see below).

## Diff semantics

Match key: `(src_file, name)`. Per match, one of:

| Class | Condition |
|---|---|
| `tp` | both resolved, `target_file` equal under extension-equivalence |
| `fp_ghost` | gnx resolved, oracle says different file (or unresolved) — **false positive edge** |
| `fp_overmatch` | gnx tier=Global with `alt_count > 0` — gnx produced N edges, oracle says 1 |
| `fn_dangling` | oracle resolved, gnx tier=Unresolved or no entry — **missed edge** |
| `tier_demoted` | both resolved to same file, but gnx tier > ImportScoped (i.e. gnx fell back to Global on something the oracle resolved deterministically) |
| `oracle_only` | oracle has an import gnx didn't see (parser miss) |
| `gnx_only` | gnx has a decision oracle didn't emit (often: gnx Tier 1 same-file calls; not a defect) |

Extension-equivalence: `a/b.ts == a/b.tsx == a/b/index.ts == a/b/index.tsx`
for TS; analogous for `.py` / `__init__.py` and Rust `mod.rs` / `lib.rs`.

## `gnx verify-resolver` CLI

```
gnx verify-resolver \
  --oracle path/to/oracle.jsonl  \
  --gnx    path/to/gnx_dump.jsonl \
  --lang   ts|py|rs              \  # selects extension-equivalence rules
  --report path/to/report.md
```

Report sections:
1. Summary table: TP / FP_ghost / FP_overmatch / FN_dangling / tier_demoted / counts.
2. Per-tier breakdown for gnx side.
3. Top-20 worst offenders by `(src_file, name)` — surfaces patterns.
4. JSON sidecar with the full classified list for downstream tooling.

Exit code: 0 always (it's a benchmark, not a test).

## Corpus (local, not committed)

| Lang | Path | Why |
|---|---|---|
| TS | `.sample_repo/TypeScript/` (NestJS) | heavy path alias + monorepo packages/* |
| Py | `.sample_repo/Python/` (requests-style) | mid-size, clean imports |
| Rs | `.sample_repo/Rust/` (tokio) | workspace with multiple member crates |

Run pattern:

```bash
# 1. dump gnx decisions
gnx admin index --repo <corpus> --dump-resolver dumps/gnx.<lang>.jsonl

# 2. dump oracle decisions
node   scripts/parity/oracles/ts_oracle.mjs <corpus> > dumps/oracle.ts.jsonl
python scripts/parity/oracles/py_oracle.py  <corpus> > dumps/oracle.py.jsonl
bash   scripts/parity/oracles/rs_oracle.sh  <corpus> > dumps/oracle.rs.jsonl

# 3. diff
gnx verify-resolver --oracle ... --gnx ... --lang ts --report report.ts.md
```

## Phases for this PR

1. **Contract** (this doc, ~150 LOC).
2. **gnx dump** in resolver.rs (~80 LOC).
3. **3 oracles** built in parallel via sub-agents (~80 LOC each).
4. **verify-resolver CLI** + diff harness (~200 LOC).
5. **Baseline run** → commit `docs/specs/2026-05-15-resolver-baseline.md`.
6. **L0 normalization** in SymbolTable (~60 LOC + tests).
7. **Post-L0 run** → append delta to the baseline doc.

L1 / L2 / L3 are explicitly out of scope for this PR.

## Out of scope (recorded for traceability)

- Vite/webpack `resolve.alias` (requires JS eval).
- LLM dynamic dispatch / reflection callsites.
- Runtime trace validation of CALLS edges (requires execution).
- C# / Java / Go oracles — same pattern can extend later.
