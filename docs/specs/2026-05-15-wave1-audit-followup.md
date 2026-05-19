# Wave-1 audit follow-up (2026-05-15)

Post-merge audit of `feat/wave-1-language-coverage` and the surrounding
analyzer surface. Three parallel reviewer agents covered (1) the 14 new
`receiver_types.rs` files, (2) `entry_points` + `config_parser`, and (3)
cross-cutting safety (unsafe / unwrap / panic / resource limits / casts).

This doc captures what was checked, what was real, what shipped, and what
was dismissed — so a future audit doesn't repeat the investigation.

## Audit dispositions

| Source | Finding | Severity (conf) | Disposition |
|---|---|---|---|
| receiver_types | Java/Kotlin/C# `or_else(Some("super"/"base"))` synthetic fallback | HIGH (90) | **Fixed** — `53a6963` |
| receiver_types | Dart `ptr::eq(id as *const u8, ...)` for node identity | HIGH (85) | **Dismissed** — stylistic, comparison is semantically equivalent to `id == id` for `usize` |
| receiver_types | Kotlin nested-fn scope leak | HIGH (82) | **Dismissed** — agent misread control flow; `continue` correctly stops the outer push loop, nested fns are skipped entirely (a different limitation, not a leak) |
| receiver_types | C# `var List<string>` generic type bind | MEDIUM (80) | **Test added** — `d59f7e4` pins intentional rejection |
| entry_points + config | csproj `..`-traversal in `<ProjectReference Include>` | HIGH (88) | **Fixed** — `4f428fb`, security |
| entry_points + config | XML comment shadowing real `<TargetFramework>` | HIGH (80) | **Fixed** — `1117752` |
| entry_points + config | csproj scan depth=2 hardcoded | MEDIUM (75) | **Fixed** — `db1c113`, env override |
| entry_points + config | entry_points decorator dup reason ("X; also: X") | MEDIUM (78) | **Fixed** — `db1c113` |
| cross-cutting | `builder::build` node accumulator `u32` overflow | HIGH (85) | **Fixed** — `c93d2f2`, u64 precompute + assert |
| cross-cutting | `builder::build` edge index range `u32` overflow | HIGH (85) | **Fixed** — `c93d2f2`, same pattern |
| cross-cutting | builder.rs `panic!("unexpected tier")` | HIGH (80) | **Verified test-only** — `c93d2f2`, tightened to exhaustive match so a new variant surfaces as compile error |
| cross-cutting | builder.rs `expect("utf-8")` on `string_pool` | HIGH (80) | **Verified test-only** — production path uses checked conversion |
| cross-cutting | embedder lock permanently poisoned after panic | HIGH (80) | **Fixed** — `54774c2`, `into_inner()` recovery |
| cross-cutting | No file size cap → OOM on rogue inputs | MEDIUM (90) | **Fixed** — `6f4012d`, `CGN_MAX_FILE_BYTES` (default 16 MiB) |
| cross-cutting | Embedding 1.2 GiB transient RAM at init | MEDIUM (85) | **POC: no win without upstream change** — `54774c2`, documented in code; fastembed's `UserDefinedEmbeddingModel::new` takes `Vec<u8>` by value, ORT has `commit_from_file(path)` but fastembed never surfaces it. Bypassing fastembed = 200+ LOC rewrite for no steady-state win. |
| cross-cutting | `row as u32` truncation (>4M lines) | LOW (75) | **Fixed** — `c93d2f2`, `calls::safe_row` saturating helper |

## What's still open (small, tracked, no action this round)

- Per-language receiver_types files still have `row as u32` direct casts at
  ~48 sites. The saturating helper `calls::safe_row` exists; migration is a
  mechanical sweep that can be done opportunistically. The truncation only
  matters on >4M-line inputs (malicious only).
- 4 dead-code warnings in `config_parser.rs` (`GlobalJsonMeta`,
  `NugetConfigMeta`, `parse_global_json`, `parse_nuget_config`) — wired in
  later Wave-2 Frameworks/Config work; left as visible TODOs.

## Tunable env vars added by this round

| Var | Default | Effect |
|---|---|---|
| `CGN_MAX_FILE_BYTES` | 16 MiB | Skip source files exceeding this size during pipeline ingest (caps worst-case worker RAM at `num_threads × MAX`) |
| `CGN_CSPROJ_MAX_DEPTH` | 4 | Directory recursion depth for `*.csproj` discovery (`.NET` monorepos commonly nest 3) |
| `CGN_EMBED_BATCH` | 32 | fastembed inference batch (already existed; documented for completeness) |

## TDD discipline applied

Every shipped fix in this round followed the eywa rule "bug fixes must begin
by writing a failing regression test before implementing the fix":

1. Write the test for the bug.
2. Run it; **must fail** on current code, otherwise dismiss the finding.
3. Apply the fix.
4. Re-run; must pass.
5. Run full workspace suite; zero regressions.
6. Commit.

This caught two of the originally-claimed findings (Kotlin nested-fn,
Dart `ptr::eq`) as misreads — the tests-first step proved the bug
didn't reproduce, so no fix shipped.

## Test count delta

| Stage | Workspace tests |
|---|---|
| Before this session (post wave-1 merge) | 539 |
| After Phase 1 (Go/Rust/Java integration tests) | 544 |
| After Phase 2-A (4 bug fixes + property test) | 547 |
| After Phase 2-B (8 audit follow-ups) | 595 |

Net: +56 tests, 0 regressions.
