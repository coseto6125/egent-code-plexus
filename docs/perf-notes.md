# Perf Notes

Empirical A/B measurements for cypher-executor changes. Each entry records a
before/after comparison of the *same* change, measured by interleaved rounds on
the same machine in the same time window (interleaving cancels machine-load
drift; absolute numbers are not comparable across entries or machines).

## FU-2026-05-24-004 — structural dedup key (`Value::write_dedup_key`)

**Change:** `dedup_rows` (cypher `RETURN DISTINCT` / non-ALL `UNION`) keyed on
`format!("{row:?}")` — one Debug-string allocation per row — replaced with a
structural byte key written into a reused buffer.

**A/B:** `before` = the commit prior to the dedup change (`format!` key);
`after` = the dedup change. Both built `--release` from the same tree, differing
only in `crates/ecp-core/src/cypher/{value.rs,executor.rs}`. Corpus:
`.sample_repo` (303,699 nodes). Wall time via `/usr/bin/time`, 8 interleaved
rounds, min reported.

| Query | before (`format!`) | after (structural) | delta |
|---|---|---|---|
| `MATCH (n) RETURN DISTINCT n.name` (303,699 rows → 153,045 distinct) | 0.16–0.17 s | 0.15 s | **~12% faster, consistent across all 8 rounds** |
| `MATCH (n) RETURN n.name` (control, no dedup) | 0.14 s | 0.14 s | **no difference** |

**Conclusion:** The structural key is measurably faster on the dedup-heavy
DISTINCT path (avoids 303k per-row Debug-string allocations). The non-DISTINCT
control shows no change, confirming the delta is attributable to the dedup path
and not to binary-wide or machine drift. Correctness: both binaries returned
identical distinct-row counts (153,045).

**Method note:** `benchmark_ecp.py` wall time includes process startup + graph
load (~20 ms fixed overhead), which swamps sub-10 ms query deltas. For a change
this size, interleaved A/B of the *full* wall on a high-cardinality DISTINCT
query (where dedup dominates) is the signal that survives machine noise — a
non-interleaved comparison against historical numbers from another machine
state is not reproducible and was explicitly not used here.
