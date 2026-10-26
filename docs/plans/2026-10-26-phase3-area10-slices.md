# Phase 3 (area 10) implementation slices

Status: deferred from 0.17.0 on 2026-10-26. Follow-up `FU-2026-10-26-4de50d62f72d`.
0.17.0 ships phase 2 and the S1 stored-data tests. No phase 2 slice depends on a slice below.

## Sources

| Key | File | Section |
|---|---|---|
| DIR | `docs/specs/2026-10-25-architecture-direction.md` | §10 (`:419-434`), phase order (`:554`) |
| C4 | `docs/specs/contracts/C4-raw-records.md` | Area 10 (`:242-250`), `BUILDER_FINGERPRINT` row (`:266`), `FetchSite` (`:281`, `:287`) |

The planning run pinned the code line numbers below on `feat/arch-p1-contracts` (`07c84140`).
Phase 2 moved many of them. Re-pin every line number against `main` before you dispatch a slice.

## Rules for every slice

- One driver owns the walk, the query pass, node construction, imports, call attachment and test classification.
- A per-language spec supplies the captures, the node-kind mapping and the rules that only that language has.
- One query pass feeds imports, schema fields, event topics and override markers.
- A driver extraction changes no output. Raw records, uids and edge multisets stay identical per language.
- Proof for an extraction: dump `LocalGraph` for the language's corpus files before and after, and diff the dumps.
- A capability addition changes output on purpose. Keep it in its own slice, and approve each difference in review.
- Call extraction for Vue, Svelte and Astro is a capability addition (`sfc_common.rs` emits no calls).
- T2 owns the driver files. T3 to T7 report a needed driver change instead of editing the driver.
- Stored data: an extraction changes none. A changed binding rule bumps `BUILDER_FINGERPRINT` (C4 `:266`).
- The driver stops binding before the node list is final (C4 Area 10: `python/parser.rs:1430` binds before `:1432-1483`).

## Slices

| Slice | Goal | Depends on | Files the agent edits | Model |
|---|---|---|---|---|
| T1 | `ECP_PROF` reports per-pass time inside `parse_file`: query pass, walk, node build, imports, calls. Today it reports per-provider totals only. | — | `crates/ecp-core/src/analyzer/pipeline.rs` (the `ECP_PROF` block); new `crates/ecp-analyzer/src/parse_prof.rs`; timer calls only in the 14 mainstream `parser.rs` files | sonnet |
| T2 | The shared driver, with Go and Rust moved onto it | T1 | new `crates/ecp-analyzer/src/driver/`; `crates/ecp-analyzer/src/go/parser.rs`; `crates/ecp-analyzer/src/rust/parser.rs` | opus |
| T3 | TypeScript and JavaScript onto the driver | T2 | `crates/ecp-analyzer/src/typescript/parser.rs`; `crates/ecp-analyzer/src/javascript/parser.rs` | opus |
| T4 | Python and Ruby onto the driver. Python runs its query twice today. | T2 | `crates/ecp-analyzer/src/python/parser.rs`; `crates/ecp-analyzer/src/ruby/parser.rs` | opus |
| T5 | Java, Kotlin and C# onto the driver | T2 | `crates/ecp-analyzer/src/{java,kotlin,c_sharp}/parser.rs` | opus |
| T6 | PHP, Swift and Dart onto the driver | T2 | `crates/ecp-analyzer/src/{php,swift,dart}/parser.rs` | opus |
| T7 | C and C++ onto the driver | T2 | `crates/ecp-analyzer/src/{c,cpp}/parser.rs` | opus |

T1 test: `test_parse_prof_disabled_records_nothing`. T2 to T7 test: the `LocalGraph` dump diff is empty for each language.

The split exists because the 14 `parse_file` bodies hold 7,899 lines, more than one run can read.

## Order

| Step | Slices | Why they can run together |
|---|---|---|
| 1 | T1 | Every later slice reads its timer. |
| 2 | T2 | It creates the driver that the others call. |
| 3 | T3, T4 | Disjoint parser files |
| 4 | T5, T6 | Disjoint parser files |
| 5 | T7 | — |

## Open item that lands in area 10

C4 `:281` and `:287`: `FetchSite` has no parse-time record. `fetch_urls::extract` returns no position, and pass 1.6 reads the file from disk at build time.
C4 recommends a parse-time `FetchSite` record (method, URL text, templated flag, site, `RawEndpoint` from `bind_enclosing`) in area 10.
That record is a capability addition with a `BUILDER_FINGERPRINT` bump. Give it its own slice after T2.
