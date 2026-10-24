# Cross-check: design A (owner split) vs design B (overlay sharing)

> Archived design record, imported on 2026-10-25. Written against `e6f56b8e`, a PR #861 commit before its squash merge; fetch it with `git fetch origin pull/861/head`. Line numbers and counts are stale: re-pin them on current `main` before a slice starts. Direction and status: [`2026-10-25-architecture-direction.md`](2026-10-25-architecture-direction.md).

Status: adversarial review. Read-only. No cargo, no test, no benchmark, no index build.
Base read: `e6f56b8e` (`git show e6f56b8e:<path>`), the same base both designs cite. Worktree HEAD is now `829ee2fe` (see F7).
Short names: A-A..A-F are design A's slices. B1..B9 are design B's PRs.

## 1. Findings, by severity

| # | Sev | Design | Finding | Conf |
|---|---|---|---|---|
| F1 | High | B | Composition rule 2 ("`NoEdge` stops the ladder **and** the constructor fallback") inverts today's index semantics. Today `resolve_call` runs the Kotlin extension retry and the constructor fallback whenever the target list is empty, whatever the reason. A literal implementation drops edges. The "pure refactor, identical edge multiset" claim for B6/B8 cannot hold as written. | 80 |
| F2 | High | B | `LexicalFunctions::targets` has a fourth outcome: `Some(vec![])` means "skip the name tiers, but still run the import tiers and the constructor fallback". `{Bound, NoEdge, Unknown}` cannot express this in B's `lexical().or_else(same_file)…` chain. Map it to `Unknown` and the name tiers run (a fabricated edge). Map it to `NoEdge` and the import tiers are lost. | 75 |
| F3 | High | A | A-C/A-E reuse the JS/TS lexical tier for Rust, Kotlin, Swift, C# and Dart, and run it before the name tiers. The candidate filter `parent.is_some() \|\| !has_owner` admits every file-level free function with scope "whole file". (a) Rust: a call inside `mod tests { fn helper(){} fn t(){ helper(); } }` binds the **file-level** `helper` at 1.0, because `tests::helper` has an owner and no parent. Today `prefer_free` prefers the module member. (b) Kotlin/Swift/Dart/C#: an implicit-`this` member `helper()` loses to a same-file top-level `helper`, because methods are never lexical candidates. | 65 |
| F4 | High | A | A-D deletes `stamp_owner_fn_by_span`. That function stamps the enclosing function name on **every** unowned node inside a function body, of any kind, not only on functions. Python's `class_definition` query is unanchored, so classes declared in function bodies are emitted. They lose their owner, so their uid changes, and same-named local classes in two test functions collide and get tombstoned (uid-collision BlindSpot). This contradicts A §5.2 ("local types … stay on their current owner strings"). | 70 |
| F5 | Med | A | Claim §4.4 "fragments an older binary wrote are never read" is false. `for_each_fragment` (overlay_reader.rs:98) decodes V2..V5 bins with no `from_current_builder()` gate. Only `accepted_fragments` (overlay_reader.rs:284) checks it. `ecp inspect` reads through it (`inspect.rs:656` → `load_overlay`). Old Rust owner strings (`A::new`, `a::b`) feed `owner_filter` matching until the dirty file is re-parsed. | 80 (claim false) / 55 (user impact) |
| F6 | Med | A | A-A is labelled "Behaviour must not change", but three of its 12 uid sites number **no** overloads today: `overlay_reader.rs:83` (`uid::compute` per symbol), `incremental.rs:291` `node_uid_for`, and the builder T7-6 skip sets (`builder.rs:3008/3045/3106/3140`). Routing them through `assign_file` changes later-overload uids in `ecp inspect` overlay output and in incremental change detection. The A-A acceptance check (a full-build graph dump) cannot see either path. Relabel A-A as a fix and add tests for those paths. | 75 |
| F7 | Med | A+B | Both designs cite `e6f56b8e`. HEAD is now `829ee2fe` (+569/−44 lines in 15 files), and 8 more files carry uncommitted WIP (+268/−3). `829ee2fe` already fixes R-F1 another way: a trait-prefixed lexical owner (`receiver_types.rs` `impl_item` arm), which changes nested-fn uids again before A-C. It also commits `rust_lexical_owner`, the `scope_defines` segment heuristic and `GoPackageLookup`, and it adds `PickFacts::namespace_visible_from` (3 impls: index.rs, view.rs, import_scope.rs). Neither design's duplication map or inventory lists that new duplicated rule. | 90 |
| F8 | Med | A×B | File overlap. A-C deletes code inside B5's move ranges (resolver.rs 1477-1501 ⊂ R7 1464-1536; view.rs 3609-3626 ⊂ 3592-3655) and inside B8's (view.rs 3558-3567 ⊂ 3531-3591). A-C/A-D change the `LexicalFunctions` input that B8 moves (builder.rs 2276-2290, 2591; view.rs 559-605). A-B adds `PickFacts::is_lexically_scoped` behind the global tier that B5 turns into `ResolveFacts::global_pick`. See §2 for the order. | 85 |
| F9 | Med | B | A live ecp run suggests the overlay and the index diverge on Rust cross-crate path calls. `ecp cypher` (merged view) has **no** call edge from `resolver.rs` (dirty in WIP) to `prefer_free_scoped` / `rust_lexical_owner`. The hook's base-graph block lists `<anonymous:1484:22>` / `<anonymous:1488:50>` callers for both. The call is `ecp_core::analyzer::same_file_pick::prefer_free_scoped(` (cross-crate path). This fits R14 (`RustWorkspaceModTree` is Cargo-aware; overlay `RustModules` is file-layout only). B keeps R14 data per side "by necessity", so B does not close it. Root cause inferred, not traced. | 60 |
| F10 | Low | B | `memo` hook: the overlay caches key on slice **addresses** (`(imports.as_ptr() as usize, imports.len())`, `heritage.as_ptr()`; view.rs:2166-2172) and store two-state values (`Option<(u32,f32)>`, `Option<u32>`). B's diagram memoizes the whole `resolve_name`, but the caches are per tier (qualified, global, named-import), and their keys miss the inputs of other tiers. Required: per-tier memo, values typed `Resolution`, a written invariant that every keyed slice outlives the view (a temporary slice reuses its address and gives a stale hit), and no RefCell borrow held across `f()`. | 60 |
| F11 | Low | B | §5.1 says `record` "becomes an inlined no-op when the dump is off". The dump is a runtime option, so the index's `record` override keeps the same `Option` check as today. That is no perf gain, but also no loss. | 60 |
| F12 | Low | A | `diff` / `review` key symbols by `SymbolKey {path, owner_class, name, kind}` (diff/symbols.rs:49-53) and keep the first (`entry().or_insert`, :246). With `display_owner`, both R-F1 `pad` nodes print `A::fmt`, so diff keeps one. This is the same as overloads today, and nothing regresses against PR 861 (one uid there), but it is not the "two nodes" that A §2 promises in every view. | 55 |
| F13 | Low | A+B | `BUILDER_FINGERPRINT = concat!("v", CARGO_PKG_VERSION, "+schema20")` (commit_meta.rs:76), so every release rebuilds anyway. A's per-slice bumps and B's "no bump" matter only for dev builds. Both designs name their next bump `schema21` for different content: assign it in merge order. | 85 |
| F14 | Low | A | Ordinals are positional: inserting `impl Debug for A { fn fmt }` above `impl Display for A` swaps `fmt#1` / `fmt#2`. Under A, every scoped child uid cascades with it. The overlay's `replaced_base` (view.rs:388) then maps a dirty node to the other impl's base node. This is pre-existing for methods; A extends it to nested functions. | 55 |
| F15 | Low | A | A-C's budget (~700 lines) includes 14 new `<lang>_lexical_scope.rs` files plus R-F1..R-F4 tests plus deletions. At ~50-80 lines per test file it exceeds the ~800-line review cap. Split the tests into their own slice. | 70 |

### Evidence

F1. resolver.rs at `e6f56b8e`:
```
785  let mut targets = resolve(type_member.as_deref().unwrap_or(site.name()), target, true);
786  if targets.is_empty() && type_member.is_some() && language == Language::Kotlin {
...      targets = self.extension_fallback(source_file, site.name(), raw_imports, target, heritage);
802  targets.retain(|&(id, _)| { self.symbol_table.member_call_admits(id, caller_owner) || module_value(id) });
806  if targets.is_empty() && !self.may_be_receiverless_member_call(source_file, site) {
807      targets.extend(self.resolve_instantiation(language, site, raw_imports, resolve));
```
Global `GlobalPick::Ambiguous` / hidden, qualified-step misses and external imports all leave `targets` empty, and line 806 still runs. B §3.2 rule 2 stops them. B §3.2 rule 4 makes `qualified` return `NoEdge(Unresolved)`, which also stops the Kotlin retry at 786. Fix: let `resolve_name` return a settled value, and treat `NoEdge` from `resolve_name` as "retry/fallback allowed" at the `resolve_call` boundary. Then only the explicit early returns (Go `Resolved`, chain `ChainBroken`) mean "stop everything".

F2. builder.rs:2591-2603:
```
let resolve = |site: CallSite<'_>| match lexical_lookup.targets(raw_node.span, site.name()) {
    Some(targets) if targets.is_empty() => {
        resolver.resolve_imported_call(&local_graph.file_path, site, &local_graph.imports)
    }
    Some(targets) => targets,
    None => resolver.resolve_call(...),
```
lexical.rs:133-136 doc: "An empty result prevents fallback to an inaccessible or ambiguous closure: only the import tiers may bind then." Fix: add `Resolution::ImportOnly` or `NoEdge::Shadowed` with a documented "import tiers + instantiation still run" rule, or pass a `tiers: TierMask` in `Lookup`.

F3. lexical.rs:29 (extension gate A removes) and :53 `if parent.is_some() || !has_owner { candidates.push((parent, start + offset as u32)); }`; methods are skipped (`if kind != NodeKind::Function { continue; }`). Today's Rust path: same_file_pick.rs:130-171 `prefer_free` → `module.and_then(|module| candidates().find(|&id| facts.owner(id) == Some(module)))` picks the `tests` member first. Fix: for languages with module or implicit-receiver scoping, admit only **scoped** candidates (parent `Some`) into `LexicalFunctions`. Leave unscoped functions to the existing name tiers. That also removes the need for A-E's import-only semantics.

F4. framework_helpers.rs:421-450 iterates `for node in nodes.iter_mut()` over every kind with `owner_class == None` and stamps the innermost Function/Method name. python/parser.rs:1483 calls it. python/queries.scm:10-15 `(class_definition name: (identifier) @class.name) @class` is unanchored. Fix: keep a kind-filtered stamp for non-callable kinds until slice F, or opt classes in to `lexical_scope_kinds`.

F5. overlay_reader.rs:98 `fn for_each_fragment(…)` dispatches `FRAGMENT_FORMAT_V2 | V3 | V4 | V5` (:139-161) with no fingerprint check. `from_current_builder` appears only at :284. Test fixtures `overlay_view_inputs.rs:484,541` read 0.15.0 and 0.16.2 bins through `load_overlay_hits`. `load_overlay_hits` has no production caller (ecp impact: 2 test callers only).

F6. overlay_reader.rs:83 `uid: ecp_core::uid::compute(kind, rel_path, owner_class, name)`. incremental.rs:284-297 `node_uid_for` → `uid::compute`. builder.rs:3008 skip set → `uid::compute`. By contrast, builder.rs:1123-1127 and view.rs:380-384 call `uid::overload(identity, ordinal)`.

F7. `git show --stat 829ee2fe` → 15 files, 569+/44−. `git diff --stat` → 8 files, 268+/3−. The `829ee2fe` hunks quoted: `receiver_types.rs` `"impl_item" => … impl_trait_name(&n, source).map(|trait_name| format!("{trait_name}::{ty}"))`, `same_file_pick.rs` `pub fn rust_lexical_owner<F: PickFacts + ?Sized>(…)`, `fn namespace_visible_from(&self, _reader: &str, _target: &str) -> bool { true }`, `import_scope.rs` `pub(crate) fn namespace_visible_from(…)`.

F9. Commands and raw output:
```
$ ECP_SKIP_BG_REBUILD=1 ecp cypher "MATCH (a)-[r:CALLS]->(b) WHERE b.name = 'prefer_free_scoped' OR b.name = 'rust_lexical_owner' RETURN a.name, a.filePath, a.startLine, b.name, r.reason, r.confidence" --repo <sessfu>
rows[2]:
  - [6]: resolve_callee,crates/ecp-core/src/session/view.rs,3572,prefer_free_scoped,l1-overlay,1
  - [6]: <anonymous>,crates/ecp-core/src/session/view.rs,3671,rust_lexical_owner,l1-overlay,1
hook (same repo): prefer_free_scoped … Called by: <anonymous:1484:22>, resolve_callee
                  rust_lexical_owner … Called by: <anonymous:1488:50>, <anonymous:3638:52>
$ git grep -n "prefer_free_scoped(\|rust_lexical_owner(" HEAD -- 'crates/*/src/**/*.rs'
resolver.rs:1490 Some(ecp_core::analyzer::same_file_pick::rust_lexical_owner(
resolver.rs:1498 ecp_core::analyzer::same_file_pick::prefer_free_scoped(
```
I report this to the coordinator as a possible ecp bug (overlay drops cross-crate Rust path calls of a dirty file). The resolver-side callers are also missing from `ecp impact --target prefer_free_scoped --direction upstream`.

F10. view.rs:1582-1608 (`QualifiedCache`, `GlobalCache` key tuples with `(usize, usize)` slice identities), :1593 `NamedImportCache = FxHashMap<(usize, ResolveTarget), FxHashMap<Box<str>, Option<u32>>>`, :2166-2188 (key build with `Box::<str>::from(callee)`; `borrow().get` released before compute, then `borrow_mut().insert`).

## 2. Proposed interleaved order

Rule: a slice that **deletes** code goes before the slice that **moves** that code. A slice that defines a shared type goes first when it touches no contested range.

| Step | Slice | Production files (from the designs, ranges confirmed at `e6f56b8e`) | Why here |
|---|---|---|---|
| 0 | PR 861 final | — | Re-run both inventories on the final commit (F7). |
| 1 | B1 outcome type + R30 helpers | new `analyzer/resolve/outcome.rs`; resolver.rs 1998-2122, 1742; view.rs 272-278, 3138-3146, 5047-5140; index.rs 64-110; heuristics.rs; import_scope.rs 189, 210 | No overlap with A. Fix F1/F2 in the type **now** (fallback-allowed `NoEdge`, import-only state), before any slice depends on it. |
| 2 | A-A `assign_file` | uid.rs, lexical.rs (stubs), builder.rs 1092-1181, 3008-3140; class_membership.rs 69; incremental.rs 291; overlay_reader.rs 75-216; view.rs 372-387 | Disjoint ranges from B. Relabel as a fix (F6). Serial after B1 only because both touch view.rs and builder.rs (project rule: no shared file in parallel). |
| 3 | B2 Go selector + G4 | resolver.rs 162-203, 718-739, 940-970; view.rs 610-635, 2912-2951, 3568-3590; reexport.rs 329+, 390-448; import_scope.rs 89-181 | Independent of A. |
| 4 | B3 chained member | resolver.rs 825-912; builder.rs 2604-2641; view.rs 583, 764-800, 2999-3120; types.rs 219 | Independent of A (adjacent to A-C's builder.rs 2591 only). |
| 5 | B4 receiver ladder | resolver.rs 2603-2807; view.rs 2797-2969; type_ladder.rs | Independent of A. |
| 6 | A-B table v16 + `display_owner` + `is_lexically_scoped` | graph.rs; commit_meta.rs; builder.rs (table write); same_file_pick.rs 54-84; index.rs 1046-1056, 1191-1202; view.rs 3148+, 3558-3567; 15 CLI display files | Must precede B5 so `global_pick`'s contract includes the scoped filter. |
| 7 | A-C Rust opt-in (fix F3 first) | rust/parser.rs, receiver_types.rs; framework_helpers.rs 347; same_file_pick.rs 64-70, 113-126, 173-214; index.rs 194-196, 627-631, 1512-1522; resolver.rs 1477-1501; view.rs 3172-3182, 3609-3626; scope_defines.rs; class_membership.rs; builder.rs 2276-2290 | Deletes the string-path code **before** B5 moves Tier 1. Otherwise B5 adds `raw_owner` / `impl_member` / `rust_lexical_owner` to `ResolveFacts` and A-C rewrites the moved copy. |
| 8 | B5 ladder skeleton (Tier 1, heritage, global) | resolver.rs 1464-1536, 1819-1877, 1892-1997; view.rs 3592-3655, 3923-3958, 3978-4050; builder.rs 2494-2530, 2748-2765 | Moves the already simplified Tier 1. |
| 9 | A-D Python / JS / TS (fix F4 first) | framework_helpers.rs 421-450, 736-767; python/parser.rs 1483; javascript/parser.rs 768; typescript/parser.rs 890; lexical.rs | Before B8, which moves the lexical tail. |
| 10 | B6 import tiers | resolver.rs 459-660, 1218-1281, 1538-1626, 1878-1891; view.rs 3960-3976, 4714-4892; import_scope.rs 21, 248-480 | F1 applies to R17 here. |
| 11 | B7 qualifier + module tree + `super()` | resolver.rs 1629-1757, 2178-2534; view.rs 2152-2419, 3840-3922 | Address F9 (R14 cross-crate) here. |
| 12 | B8 call entry + lexical tail + constructor fallback | resolver.rs 705-824, 913-1060; view.rs 586-763, 1443-1477, 3531-3591, 4313-4388; builder.rs 2591 | Models the final lexical semantics once (after A-C/A-D). |
| 13 | B9 variants, supertypes, namespace owners | builder.rs 2298-2360, 2429-2493; index.rs 485-711, 1287-1336; view.rs 3192-3224, 3372, 4399-4696 | Independent of A. |
| 14 | A-E Kotlin / Swift / C# / Dart | lexical.rs table; per-language parsers if a capture is missing | Last: needs the F3 implicit-this rule, which is written once in the shared ladder. |

Which program goes first: **B1 first** (a type, no contested range), then A-A. Of the contested pairs, **A-C precedes B5 and A-D precedes B8**: deleting before moving saves one full rewrite of Tier 1 and the lexical tail, plus two `ResolveFacts` signature changes. B's `Resolution` changes no signature that A-A..A-D call. A calls `LexicalFunctions::targets`, `prefer_free` and `PickFacts`, and B changes those only in B5/B8, which come after A-C/A-D in this order.

## 3. Claims verified (one line each)

| Claim | Verdict | Evidence |
|---|---|---|
| 2a: two `impl … for A { fn fmt }` get distinct uids | True | queries.scm:45-68 make every impl `function_item` `@method`; builder.rs:1123-1127 `keeps_overloads` → `uid::overload(uid_u64, ordinal)`; index.rs:767-775 `register_overload` returns `later.len()`. |
| 2a: same ordinal index vs overlay view | True | view.rs:380-384 numbers per identity in `file.symbols` order. FragmentSymbol order = `LocalGraph.nodes` order (overlay_writer.rs:116-136), as in builder Pass 1. |
| 2a: same ordinal in overlay **reader** / incremental / T7-6 | False today | see F6. |
| 2a: stable across re-index | True for unchanged files | positional; see F14. |
| 2b: fragments hold every span `lexical_parents` needs | True | FragmentSymbol {name, kind, span, owner_class, …} for every `graph.nodes` entry; Rust Module (queries.scm:133-135), Impl (:162-168), argument closures (:176) are nodes. |
| 2b: "old fragments never read" | False | F5. |
| 2c: old graph generations and caches are cleaned | True, with a 7-day lag | `sweep_old_format_commits` gc.rs:426 via `older_graph_format` :463 (`version < GRAPH_FORMAT_VERSION`); `sweep_stale_parse_caches` :384; `sweep_stale_generations` :611 and `drop_superseded_generations` admin/index.rs:443; `sweep_sessions` :298; `sweep_stale_fragments` :507; auto-run `admin gc --auto` from auto_ensure.rs:986 and hook/session_start.rs:212; `IDLE_GRACE_SECS = 7 * 86_400` gc.rs:43. A v16 root (+1 vector) stays inside `GRAPH_ROOT_TAIL_BYTES = 4096`. |
| 2d: every tier returns ≤ 1 target | True | `resolve_symbol_with_import_binding` pushes once and returns at 1525/1715/1743/1770/1862/1929/1951; `import_scoped` `vec![(node_id, confidence)]` :1345; `resolve_instantiation` returns `Option`, destructures `&[(type_id, confidence)]` :1044; JS lexical `vec![(target, 1.0)]` lexical.rs:159. The only multi-target emitter is `emit_overload_references` (builder.rs, `References` first→later), and it is not a resolver return. `Bound(Hit)` loses nothing. The barrel `via` out-param and `alt_count` (dump only) need a side channel; B notes `via`. |
| 2d: `Resolution` is 12 bytes | Plausible, unverified | Hit = u32 + f32 + u8 → 12 with padding; niche in `Tier` (11 of 256 values); the `const` assert guards it. |
| 2e: view cache keyed on the binary | True | view_cache.rs:35-47: `VIEW_CACHE_FORMAT`, `BUILDER_FINGERPRINT`, `ECP_GIT_SHA`, exe path+len+mtime, graph path+len+mtime, node count, `inputs_hash`. |
| 2e: refactor slices need no bump | True in effect | fingerprint embeds the package version (F13); risk only for dev graphs. |
| 2f: overlay caches `qualified` / `global` / `named_imports` exist | True | view.rs:1643, 1647, 1650; keys :1582-1608; see F10 for key semantics. |
| B: confidences equal on both sides | True | heuristics.rs:43-57 vs view.rs:272-278; ladder maps resolver.rs:2633-2655 vs view.rs:3138-3146. |
| B: facts traits used without `dyn` | True | `git grep "dyn ModuleFacts\|dyn PickFacts\|dyn TypeFacts"` → no match. |
| A: graph.bin has no per-file node range | True | ZeroCopyGraph field list graph.rs:1140-1234 has none. |
| A: `stamp_owner_fn_by_span` callers | True | ecp impact: `stamp_js_function_owners`, Python `parse_file` (d=1), JS/TS `parse_file` (d=2). |

## 4. Gaps: findings neither design fixes

| Finding | Status after both programs |
|---|---|
| G2 (b) nested `go.mod` / no root `go.mod` | Open. B's `Package(Unknown)` keeps the 0ca30699 fallback only for an unmapped package. Neither design discovers nested modules. |
| G3 Go namespaces make same-named type methods "free" (index side) | Open. B9 moves R28 "with no behaviour change". B itself notes that WIP fixes only the overlay side, which is a new parity gap. |
| G5, G6, G7 (Go parser locals / `s.f()()` / extra import rows) | Open (parser). |
| G8 (fingerprint doc) | Open (trivial). |
| R-F5 (`name_ref` uses owner text) | Optional in A (Q6). Not committed. |
| R-F6 (positive fallback test) | Neither adds it. B defers to PR 861 fixtures. |
| P-F2 | Fixed in `829ee2fe` by `namespace_visible_from`. That adds a new duplicated rule B must absorb (F7). |
| P-F3 (Python missing-module receiver) | Open. |
| P-F4 (old-manifest exit untested) | Open, and A **depends** on that exit (§4.4). A-C's PR-861-written fixture should assert the `from_current_builder` exit **and** the `ecp inspect` path (F5). |
| P-F1, P-F6 | Being fixed by Codex (WIP `php/parser.rs` body-span check; `829ee2fe` skips anonymous-class body nodes). The P-F6 class (lexical owner of local **types**) stays possible under A: local types are excluded until slice F. |
| G1, C1, R-F1..R-F4, G4 | Covered: G1/C1 by PR 861 plus B's explicit states; R-F1..R-F4 by A; G4 by B2. |

Structurally impossible after the programs: only R-F2 (one derivation function) and the R-F1 collision (parent-uid hashing). The "empty means two things" class (G1/C1) becomes explicit but not impossible: every converted site still chooses `require` or `admit` by hand, and F1/F2 show that the rules as written choose wrong.

## 5. Project-rule check

| Rule | A | B |
|---|---|---|
| 14-language coverage | A-C adds 14 per-language files: compliant. A-A changes the uid primitive (a graph-construction primitive) with corpus parity only and no per-language tests: weak. | B1 adds a 14-language parity table: compliant. It tests overlay against reindex, not index edges against the previous binary per language; the corpus diff (§4.3.3) covers that. |
| rkyv append-only discriminants | No NodeKind/RelType added; the new root field is behind a format bump: compliant. | No schema change. |
| Stored data written by the previous binary | A-B (v15 graph from the A-A binary) and A-C (session written by the PR 861 binary): compliant in plan. The fixture must also go through `load_overlay` (F5). | N/A (no stored shape changes). |
| Per-query < 30 ms | No violation: `assign_file` + one sort per dirty file; `display_owner` is one binary search per printed row. | No violation by design. Risk lives in B7/B8 (exact R11 probing, `super()`), which B names. Needs the dirty-file bench that B proposes before B2. |

## 6. Effort

| Program | Designs' own estimates | Realistic |
|---|---|---|
| A (A-A..A-E) | 400 + 500 + 700 + 500 + 500 = 2,600 | ~3,200 (A-C tests alone ~800-1,000; F4/F3 fixes add code) |
| B (B1..B9) | 450 + 600 + 550 + 700 + 750 + 800 + 750 + 700 + 700 = 6,000 | ~7,000 (F1/F2 type changes; B6 likely splits) |
| Total | 8,600 | ~10,000 changed lines |

Riskiest single slice: **B8** (call entry + constructor fallback + lexical tail). It rewrites the entry of every call edge in all 14 languages on both sides, and the F1 and F2 contradictions surface exactly there. Runner-up: **A-C**, which combines a uid change, a stored-data migration, a semantics change of the lexical tier for Rust (F3), and deletion of code that B wraps.

## 7. Blind spots

- Nothing ran except `git show`/`git grep`/`git diff` and `ECP_SKIP_BG_REBUILD=1 ecp impact|find|cypher` on the sessfu worktree. Those ecp calls printed `l1.refreshed written=11` / `written=2` and `session.promoted case=A`: they wrote session fragments under `~/.ecp`, but built no index.
- F1-F4 are inferred from source, not from a fixture run. F3(b) assumes Kotlin/Swift/Dart/C# bare calls reach Tier 1 with the caller owner, as on other languages. I did not read the Kotlin `reaches_free_only` arm.
- F9's root cause (R14 data) is inferred. I did not read `RustModules` (view.rs 4090-4312) or `try_module_tree_resolve`.
- Not read: `view.rs` outside the cited ranges, `resolve_qualifier_file`, `import_member_hit`, `name_ref.rs` beyond the `namespace_visible_from` call, the Kotlin/Swift/Dart/C# query files, `promotion.rs`, `merged.rs`, `cypher/executor.rs`.
- Line numbers are at `e6f56b8e`. HEAD `829ee2fe` plus WIP shift resolver.rs and view.rs by up to ~60 lines.
- The 12-byte `Resolution` layout is not compiled.
- Temporary source copies were written to a scratchpad dir outside the design folder and deleted afterwards.
