# Design B: one resolution rule set for the index and the session overlay (v2)

> Archived design record, imported on 2026-10-25. Written against `ff75103a`, a PR #861 commit before its squash merge; fetch it with `git fetch origin pull/861/head`. Line numbers and counts are stale: re-pin them on current `main` before a slice starts. Direction and status: [`2026-10-25-architecture-direction.md`](2026-10-25-architecture-direction.md).

Status: DESIGN v2 (no code written, no cargo run). v1 is not kept.
Base: committed HEAD **`ff75103a`** on `fix/session-followups` (PR 861 work: `829ee2fe`, `f35f62d8`, `ff75103a` after v1's base `e6f56b8e`).
Every file:line below is at `ff75103a` (`git show ff75103a:<path>`). PR 861 can still move. Before each slice starts, re-pin its ranges on the PR 861 final commit with the pin script in §7.
The worktree had 3 uncommitted files again at the time of reading (`chained_call_return_type.rs`, `owner_class_rust.rs`, `same_file_pick.rs`). This design does not read them.

## 0. Summary

- The index and the overlay run the same tier ladder: Lexical, SameFile, ImportScoped, QualifierScoped, ModuleTree, receiver ladder, HeritageScoped, wildcard, Global / NearestCopy. After the ladder come three call-level steps: the Kotlin extension retry, the untyped-member post-filter and the constructor fallback.
- The leaf rules are partly shared already (67 helpers are called from both sides). The ladder that orders them, and 24 helpers plus 1 new rule (`namespace_visible_from`), exist twice.
- Both sides return one value for two different meanings. The index returns an empty `Vec<(NodeId, f32)>` and the overlay returns `None`. Each can mean "no edge, stop" or "unknown, try the next rule". G1, G2 and C1 are this defect.
- v2 changes after the cross-check ([`2026-10-23-architecture-designs-crosscheck.md`](2026-10-23-architecture-designs-crosscheck.md)):
  1. **Two stop levels.** `NoEdge` stops the *ladder* only. The call-level fallbacks still run after it, as on the index today (F1). Only a short, named list of reasons (`Final`) also stops the call-level fallbacks.
  2. **A fourth state `Shadowed`** is in the type from slice B1 (F2).
  3. **New rules** from `829ee2fe` / `f35f62d8` are in the map: `namespace_visible_from`, `rust_lexical_scope`(s), the chained-member import arms, and Go namespace owners (F7).
  4. **The interleaved order with design A** is adopted. Each B slice names the A slices it depends on (F8).
  5. **A Rust cross-crate overlay gap (R14)** is confirmed by ecp and gets its own slice (F9).
  6. **The memo rules are written down** (F10). The perf claim about `record` is corrected (F11). `schema21` is assigned in merge order (F13).

## 1. Duplication map

### 1.1 How the map was built (ecp commands and counts)

All commands ran with `ECP_SKIP_BG_REBUILD=1 … --repo <PR 861 worktree>`. No call printed an index build. The calls did print `l1.refreshed written=2 failed=0`, which means they refreshed session fragments.
The ecp graph reflects the working tree at query time (`ff75103a` plus 3 dirty test/WIP files). Line numbers in the tables come from `git show ff75103a:…`.

| Question | Command | Result (v2 run) |
|---|---|---|
| Shared `analyzer/` helpers the overlay calls | `ecp cypher "MATCH (f)-[:CALLS]->(g) WHERE f.filePath = 'crates/ecp-core/src/session/view.rs' AND g.filePath STARTS WITH 'crates/ecp-core/src/analyzer/' RETURN count(DISTINCT g.name), count(*)"` | 70 helpers, 155 calls |
| Same, from the index | same, `f.filePath STARTS WITH 'crates/ecp-analyzer/src/resolution/'` | 67 helpers, 140 calls |
| Helpers called by **both** sides | `ecp cypher "MATCH (a)-[:CALLS]->(g), (b)-[:CALLS]->(g) WHERE a.filePath = '…/view.rs' AND b.filePath STARTS WITH '…/resolution/' AND g.filePath STARTS WITH '…/analyzer/' RETURN count(DISTINCT g.name)"` | 67 |
| Function names on both sides | two `ecp cypher "MATCH (b) WHERE (b:Function OR b:Method) AND b.filePath … RETURN DISTINCT b.name" --format json`, intersected locally (overlay = view.rs + import_scope.rs; index = resolution/*) | overlay 192, index 341, common 56. Of these, 32 are facts-trait methods (two impls are expected) and 24 are duplicated rules |
| Go rule callers | `ecp impact --target go_package_lookup --direction upstream` | d=1: `Resolver::resolve_call`, `resolve_callee` (view.rs:3572), `go_imported_package_member` |
| Old Go wrapper | `ecp impact --target go_imported_package_member --direction upstream` | 0 callers: a dead wrapper, deleted in B2 |
| Chained member | `ecp impact --target resolve_chained_member --direction upstream`; `… --target chained_member --file crates/ecp-core/src/session/view.rs` | index 1 caller (builder chain closure); overlay 1 caller (view.rs chain closure) |
| How the overlay reaches the ladder | `ecp path resolve_callee member_via_type` | `resolve_callee -> receiver_ladder -> member_via_type` |
| Index API callers | `ecp impact --target <n> --file crates/ecp-analyzer/src/resolution/resolver.rs --direction upstream` (d=1) | `resolve_symbol` 30, `resolve_imported_symbol` 2, `resolve_symbol_with_heritage` 3, `resolve_import` 2, `resolve_import_binding` 1. Cross-check: `git grep -F '.resolve_symbol('` in `crates/*/src` gives 32 |
| `namespace_visible_from` | `ecp find namespace_visible_from` (4 candidates); `ecp impact --target namespace_visible_from --file crates/ecp-core/src/analyzer/same_file_pick.rs --direction upstream` | 4 definitions; **0 callers** (see A4). `git grep` shows 1 caller: `name_ref.rs:143` |
| R14 Rust cross-crate | `ecp cypher "MATCH (a)-[r:CALLS]->(b) WHERE b.name = 'prefer_free_scoped' OR b.name = 'rust_lexical_owner' RETURN a.name, a.filePath, a.startLine, b.name, r.reason, r.confidence"` | with resolver.rs **clean**: 4 rows, incl. `resolver.rs:1501 -> prefer_free_scoped` and `resolver.rs:1505 -> rust_lexical_owner` (reason `call`, 1.0). The cross-check's run with resolver.rs **dirty** had only the 2 view.rs rows. This confirms F9 (§1.4) |

ecp anomalies (report as ecp bugs):
- **A1.** `impl<'a> ModuleFacts for OverlayFacts<'a>` (view.rs:2438) and `impl PickFacts for OverlayFacts<'_>` (view.rs:3175) produce no implements edges.
- **A2.** The call `self.scope.files_in_directory(..)` (a method call through a struct field) is unresolved.
- **A3.** A two-variable name join in cypher returns no rows.
- **A4 (new).** A trait-method call on a generic receiver (`facts.namespace_visible_from(..)`, where `F: PickFacts`; name_ref.rs:143) links to no definition.
- **F9.** The overlay drops cross-crate Rust path calls (§1.4).

### 1.2 Rules implemented twice (at `ff75103a`)

Status: **dup** = two implementations of one rule. **dup-name** = the same name on both sides (from the ecp intersection). **shared** = one rule in `ecp-core/src/analyzer/`, two call sites. **diverge** = the sides differ. "A-x" = design A's slice that also edits this range.

| # | Rule | Index | Overlay | Status | Bugs / notes |
|---|---|---|---|---|---|
| R1 | Call entry: normalize site, ladder, Kotlin retry, post-filter, constructor fallback | `resolver.rs:705-818` (`resolve_call`; Kotlin retry 794, retain 810, fallback 814-816) | build closure `view.rs:586-763` (Kotlin 711, untyped filter 731, fallback 739); `resolve_callee` `view.rs:3572-4110` | dup | G1 fixed in PR 861 by `GoPackageLookup`; the overlay sets `scope.call.untyped/free_only/heritage` by hand (view.rs:3591-3620) |
| R2 | Site normalization (Swift / Python untyped, typed receiver) | `resolver.rs:749-767`, `type_receiver` `965-990` | build `view.rs:610-640`; `type_receiver` `view.rs:2922-2961` | dup, dup-name | – |
| R3 | Go package selector | call `resolver.rs:723-747` | call `view.rs:3591-3620` | shared rule `reexport.rs:461` `go_package_lookup` + `GoPackageLookup` `453`; dead wrapper `go_imported_package_member` `435` | G2(b) nested `go.mod` open; G4 (overlay scan) open |
| R4 | Python submodule member | `resolver.rs:715-722`, `python_submodule_member` `1307` | `view.rs:3620-3632`, in `type_receiver` | shared `reexport.rs:877` | – |
| R5 | Python declared attribute | `resolver.rs:749-757`, `1013-1022` | build `view.rs:618-628` | shared `reexport.rs:965` | P-F3 open |
| R6 | JS/TS lexical scoping, then import-only path | `builder.rs:2592-2596` -> `resolve_imported_call` `resolver.rs:996-1039` | build `view.rs:590-605` | `lexical.rs:70` shared; tail **diverge** | **L1 (unverified):** index runs Python submodule, declared attribute, `resolve_import_tiers` and `resolve_instantiation`; overlay runs ES `named_import` only. A-D changes this input |
| R7 | Tier 1 SameFile + owner pick + Rust lexical owner | `resolver.rs:1489-1557` (`rust_lexical_owner` 1505, `prefer_free_scoped` 1515) | `view.rs:3652-3720` (`rust_lexical_owner` 3671) | pick rules shared (`same_file_pick.rs:102,164,214,310`); ladder code dup | A-C deletes code here (resolver.rs ~1500-1520, view.rs ~3665-3690) |
| R8 | Tier 2 import-member: explicit, retry, extension, external | `resolver.rs:1559-1649`, `import_member_hit` `1243-1302`, `import_is_external` `1334` | `bind_import` `view.rs:4814-4881`, `import_member_hit` `4882-4952`, `ImportBinding` `4774` | dup, dup-name | – |
| R9 | Module discovery | `import_module_file` `resolver.rs:459`, `discover_import_module` `533`, `discover_fqn_module` `591`, `IndexedModule` `71` | `ImportScope::module` `import_scope.rs:271`, `python_module` `360`, `fqn_module` `396`, `Module` `21` | dup | – |
| R10 | Tier 2 ES named import | `resolve_named_import` `resolver.rs:1202-1235` | `named_import` `view.rs:1923` | shared `reexport.rs:677` | – |
| R11 | Tier 2 import for other languages | `resolve_named_import` (exact probe) | segment approximation `view.rs:3822-3898`, `relative_module_rank` `5075`, `path_matches_segment` `5212` | **diverge** (by design) | unmeasured |
| R12 | Python `super().m()` | `resolve_super_member` `resolver.rs:2479` | no edge `view.rs:3914-3917` | **diverge** | – |
| R13 | Tier 2.5 qualifier file + member | `resolver.rs:1650-1762`, `resolve_qualifier_file` `2199-2478` | `qualified_callee` `view.rs:2162-2268`, `qualifier_file` `2269-2437` | dup | – |
| R14 | Tier 3.5 Rust module tree | `resolver.rs:1763-1778` (`MT_CONF` 1763), `try_module_tree_resolve` `2934` (Cargo-aware `RustWorkspaceModTree`) | build `view.rs:675-700`, `RustModules` `4150-4321` (roots = ancestors of dirty callers only), `resolve_rust_module_callee` `4322` | dup + **diverge** | **F9 confirmed:** the overlay cannot resolve `other_crate::path::f()` |
| R15 | Receiver ladder + tier mapping | `resolve_member_via_type` `resolver.rs:2624-2688` (map 2655) | `receiver_ladder` `view.rs:2962`, `ladder_edge` `3165` | core shared `type_ladder.rs:323`; map dup | – |
| R16 | Type candidates | `type_candidates` `resolver.rs:2689`, `csharp_alias_type` `2804` | `view.rs:2807`, `2897` | dup, dup-name | – |
| R17 | Kotlin extension retry (ladder and call) | ladder `resolver.rs:1806-1822`, `extension_fallback` `1379`; call `794-797` | ladder `view.rs:3940-3980`; call `view.rs:711-724` | dup | – |
| R18 | Tier 2.75 heritage | `resolver.rs:1840-1898` | `view.rs:3983-4019` | dup | – |
| R19 | Wildcard after heritage | `resolver.rs:1899-1912` | `view.rs:4020-4037` | dup | – |
| R20 | Tier 3 Global / NearestCopy / import-hides | `resolver.rs:1913-2017`; `lookup_global` `index.rs:1055`, `nearest_global_copy` `1205`, `nearest_copy` `1172` | `view.rs:4038-4110`; `nearest_copy` `view.rs:2067` | filter shared (`file_category.rs:425`, `reexport.rs:1057`); nearest copy dup | A-B adds a scoped filter here |
| R21 | Untyped-member post-filter | `resolver.rs:798-812` | `view.rs:725-737` | predicate shared `same_file_pick.rs:471` | – |
| R22 | Constructor fallback | `resolve_instantiation` `resolver.rs:1051-1085`, `names_constructible_type` `1086`, `may_be_receiverless_member_call` `938` | `resolve_constructed_type` `view.rs:4373`, `construction_target` `1443`, `sole_constructor` `1463`, `may_be_receiverless_member_call` `3148` | dup, dup-name | – |
| R23 | Chained member | `resolve_chained_member` `resolver.rs:833-925` (`_ => return None` 912; Java/Kotlin non-unique `return None` 863); chain closure `builder.rs:2610-2647` (`chain_failed` 2610) | `chained_member` `view.rs:3009-3132` (`_ => return None` 3111); `declares_type_beside` `3133`; chain closure `view.rs:583`, `765-800` | walk shared `types.rs:219`; rule dup | **C1 residual:** `f35f62d8` adds Rust/PHP import arms and C++/Dart `imported_file_type` (reexport.rs:388) on both sides. `_ => return None` still drops C, Go, and Rust/PHP without a naming import |
| R24 | Field reads | `builder.rs:2745-2765` (`ResolveTarget::Field` 2753); import tiers off `resolver.rs:1563` | `view.rs:908-945` (`field_read` 939); import tiers off `view.rs:3723`, `3824` | ladder dup | O1 accepted |
| R25 | Name refs | adapter `builder.rs:2495` | adapter `view.rs:3443` | shared `name_ref.rs:26` | P-F2 fixed by R31 |
| R26 | Enum variant refs | `builder.rs:2430`, `index.rs:1309`, `1328` | `view.rs:4499`, `4573`, `1845` | dup, dup-name | – |
| R27 | Supertypes pre-pass | `builder.rs:2299-2360` | `dirty_supertypes` `view.rs:4616`, `resolve_type` `4713` | dup | – |
| R28 | Namespace owner | `note_namespace` `index.rs:492` (Go excluded at 499), `build_owner_kinds` `694`, `is_namespace_owner` `1558` | `is_namespace_owner` `view.rs:3225` (Go returns false), `namespace_owners` `3413` | dup | G3 now fixed on **both** sides (`829ee2fe`); the cross-check §4 note is out of date |
| R29 | Tier confidences | `heuristics.rs:43`, `MT_CONF` `resolver.rs:1763` | `CONF_*` `view.rs:272-278` | dup | – |
| R30 | Small helpers | `split_qualifier` `resolver.rs:2111`, `qualifier_prefix_is_internal` `2019`, `leaf_path` `index.rs:104`, `path_dirs` `112`, `crate_root_prefix` `66/74`, `owner_path_agrees` `1029`, `rust_module_file` `resolver.rs:2907`, `rust_type_path_agrees` `2847`, `files_by_stem` `index.rs:357`, `namespace_return_type_visible` `377`, `namespace_type_path_matches` `402` | `view.rs:5107`, `5133`, `5185`, `5193`, `5150`, `2784`, `1863`, `1879`, `2124`; `import_scope.rs:206`, `233` | dup, dup-name | `crate_root_prefix` differs on Windows |
| **R31 (new)** | PHP namespace visibility at the global name-ref tier | `PickFacts::namespace_visible_from` impl `index.rs:1566` (`file_namespaces`) | `view.rs:3264` delegates with `Language::Php` to `ImportScope::namespace_visible_from` `import_scope.rs:157` (`LanguageFiles.namespaces`, built for FQN languages + C#; PHP is FQN) | trait default `same_file_pick.rs:87` (`true`); **3 impls, 2 bodies duplicated** | Fixes P-F2. One caller: `name_ref.rs:143`. Parity holds only while the single caller asks about PHP files |
| **R32 (new)** | Rust lexical scope of a node | store `rust_lexical_scopes` (`index.rs:203`), filled by `note_rust_lexical_scopes` `index.rs:736` (called `builder.rs:1266`); impl `index.rs:1546` | store `dirty_rust_lexical_scopes` (`view.rs:1757`); impl `view.rs:3211` returns `None` for base ids | builder shared `same_file_pick.rs:545` (`rust_lexical_scopes`); stores dup | Invariant: only `rust_lexical_owner(caller)` reads it, and an overlay caller is always dirty. A new reader for candidate ids would break parity. A-C replaces this |
| **R33 (new)** | Chained return type via imports (Rust/PHP alias, C++/Dart file) | `resolver.rs:889-908` | `view.rs:3088-3108` | rule `reexport.rs:388` shared; arm code dup | part of R23 |

Totals: 33 rule rows. Shared as a rule: R3, R4, R5, R10, R25, plus the R6 head. Duplicated: 22. Diverging: R6 tail, R11, R12, R14. The G3 divergence that v1 listed under R28 is fixed.

### 1.3 Already shared

67 helpers are called from both sides (§1.1). The largest groups are `import_binding.rs`, `reexport.rs` (`pick_call`, `named_import`, `import_hides`, `go_package_lookup`, `python_*`, `imported_file_type`), `same_file_pick.rs` (`prefer_owned`, `prefer_free_scoped`, `member_call_admits`, `rust_lexical_owner`, `rust_lexical_scopes`, …) and `type_ladder.rs` (`member_via_type`, `returned_type`, …).

### 1.4 R14 / F9: the Rust cross-crate path gap

- **Index:** `try_module_tree_resolve` (resolver.rs:2934) reads the Cargo workspace tree. A call such as `ecp_core::analyzer::same_file_pick::prefer_free_scoped(..)` binds at 1.0.
- **Overlay:** `RustModules::new` (view.rs:4150-4200) keeps as roots only the `lib.rs` / `main.rs` / target roots whose directory is an ancestor of a dirty caller, which is the caller's own crate. No crate-name table exists.
- **Path aliases:** `config_parser.rs:66-87` maps only the root `Cargo.toml` package name. Workspace members (`ecp_core`) get no alias.
- **Effect:** while a file is dirty, `ecp impact` loses its calls into other workspace crates. The ecp run in §1.1 shows this on this repo.
- **Fix (slice B7b, a behaviour change in the overlay only):**
  - Persist the workspace crate table (`crate name -> root file`) that the index already builds. It goes into `graph.bin` beside the path aliases and needs a `GRAPH_FORMAT_VERSION` bump. Share that bump with A-B's v16 if both land in one release.
  - `RustModules` then resolves crate heads from that table.
  - Index edges do not change.

## 2. Facts boundary

### 2.1 What each side holds

This is unchanged from v1 §2.1, with these additions:
- index `rust_lexical_scopes` vs overlay `dirty_rust_lexical_scopes` (R32);
- index `file_namespaces` vs overlay `LanguageFiles.namespaces` for R31.

The overlay cannot build a `SymbolTable`: materializing `graph.bin` breaks the 30 ms budget. So the two fact stores stay, and the ladder is shared.

### 2.2 Traits

These are the same as v1 §2.2, and generic everywhere (`git grep "dyn ModuleFacts\|dyn PickFacts\|dyn TypeFacts"` finds no match today):
- **`ModuleFacts` additions:** `for_each_file_in_directory` (visitor), `file_declares_namespace`, `namespace_declared`.
- **New `ResolveFacts: ModuleFacts + TypeFacts`:** `import_module`, `import_is_external`, `files_by_stem`, `rust_module_file`, `rust_module_tree_member`, `global_pick`, `nearest_global_copy`, `has_constructible_type`, `names_admitted_method`, `sole_constructor`, `enum_variant_of`, `memo`, `record`.

v2 changes:

1. **`namespace_visible_from` moves to one body.** `ResolveFacts` gets `fn file_namespaces(&self, language, file) -> impl Iterator<Item = &str>`. `namespace_visible_from` becomes a free function in `same_file_pick.rs` over that iterator. The duplicated bodies (`index.rs:1566`, `import_scope.rs:157`) go. The overlay's hard-coded `Language::Php` becomes the language of `reader`. This happens in B1 and changes no behaviour, because the only caller is PHP-only.
2. **`rust_lexical_scope` stays as is.** Design A (A-C) replaces it with lexical parents. B5 does not move it into `ResolveFacts`, because A-C lands before B5 (§4.2).
3. **`global_pick` takes the scoped filter that A-B adds** (`is_lexically_scoped`). B5 lands after A-B and adopts that contract.
4. **Memo is per tier, not per ladder (F10).** The signature is `fn memo(&self, tier: MemoTier, key: MemoKey<'_>, f: impl FnOnce() -> Resolution) -> Resolution`, with `MemoTier ∈ {Qualified, Global, NamedImport}`. Invariants:
   - (a) The overlay keeps its keys exactly (`view.rs:1582-1608`). That includes the slice-address parts `(imports.as_ptr(), len)` and the heritage pointer. Every slice used in a key must outlive the `OverlayView::build` call. Only `file.imports` of a dirty input and `enclosing_class_heritage` slices qualify. A temporary `Vec` must never be a key part; an empty slice is keyed by `(0, 0)`.
   - (b) No `RefCell` borrow is held across `f()`. The pattern is: borrow, get, drop the borrow, compute, then `borrow_mut` and insert (as at view.rs:2176-2198).
   - (c) The cached values become `Resolution`. A memo hit returns the stored state as it is, so a cached `NoEdge` stays `NoEdge`.
   - (d) The index's `memo` is `f()`.
5. **`record` is not a perf claim (F11).** The index override keeps today's `Option` check, because the dump is a runtime option. The claim is "no loss", not "gain".

## 3. Outcome type

### 3.1 Definition

```rust
// crates/ecp-core/src/analyzer/resolve/outcome.rs (new in B1)

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Tier {
    Lexical, SameFile, ImportScoped, QualifierScoped, ModuleTree,
    TypeOwned, TypeHeritage, TypeCandidates, HeritageScoped, Global, NearestCopy,
}
// Tier::confidence(): as v1 (1.0 / 0.95 / 0.85 / 0.8 / 0.7 / NEAREST_COPY_CONFIDENCE).

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit { pub id: u32, pub confidence: f32, pub tier: Tier }

/// Why a rule settled on no edge. Each reason has a fixed reach (§3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NoEdge {
    // Reach::Ladder — stops the remaining tiers only; call-level fallbacks still run.
    Unresolved, Ambiguous, External, Hidden, Rejected,
    // Reach::Call — stops the ladder and every call-level fallback.
    PackageMissing,      // Go selector names a mapped package that lacks the member (resolver.rs:725-740)
    DeclaredAttribute,   // Python `from m import a; a.f()` on the import-only path (resolver.rs:1013-1022)
    ChainBroken,         // a typed chain level found no member after its Kotlin retry (builder.rs:2610)
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[must_use]
pub enum Resolution {
    Bound(Hit),
    NoEdge(NoEdge),
    /// This rule does not apply. The next tier runs.
    Unknown,
    /// A lexical scope of the file declares the name, but none in reach binds it
    /// (`LexicalFunctions::targets` == `Some(vec![])`, lexical.rs:68-70). Skip the
    /// name tiers. The import tiers and the call-level fallbacks still run.
    Shadowed,
}

impl NoEdge {
    pub const fn reach(self) -> Reach {
        match self {
            NoEdge::PackageMissing | NoEdge::DeclaredAttribute | NoEdge::ChainBroken => Reach::Call,
            _ => Reach::Ladder,
        }
    }
}

const _: () = assert!(std::mem::size_of::<Resolution>() <= 12);
```

The size is still 12 bytes: `Shadowed` and `Unknown` are fieldless, and `NoEdge` has a 1-byte payload. If rustc lays it out larger, flatten the reasons into fieldless variants; the `const` assert fails first.

### 3.2 Which state triggers which fallback (F1)

Today's index, verified at `ff75103a`:
- `resolve_call` runs the Kotlin retry when `targets.is_empty() && type_member.is_some() && Kotlin` (resolver.rs:794-797).
- It then filters with `retain` (810).
- It runs the constructor fallback when `targets.is_empty() && !may_be_receiverless_member_call` (814-816). Neither step asks *why* the list is empty.
- Only explicit `return`s before the ladder skip these steps: the Go `Resolved` early return (725-740) and the Python declared attribute on the import-only path (1012-1020).
- The chain driver applies its own Kotlin retry (builder.rs:2623-2638), then sets `chain_failed`.

The v2 rule reproduces this exactly:

| State reaching the step | Later ladder tiers | Kotlin retry (call, R17) | Untyped post-filter (R21) | Constructor fallback (R22) | Outer chain level |
|---|---|---|---|---|---|
| `Bound` | skip | skip | applies (`admit`: refused -> `Unknown`) | skip, unless the post-filter emptied it | typed via `chained_member` |
| `NoEdge(r)`, `r.reach() == Ladder` | skip | **runs** (when `type_member` and Kotlin) | n/a | **runs** (unless `may_be_receiverless`) | `Unknown` -> level fallback |
| `NoEdge(r)`, `r.reach() == Call` | skip | skip | n/a | skip | `NoEdge(ChainBroken)` |
| `Unknown` | run | runs | n/a | runs | `Unknown` -> level fallback |
| `Shadowed` | **skip name tiers, run import tiers** (`resolve_imported_call` path) | skip (that path has none today) | n/a | runs | `Unknown` -> level fallback |

Consequences:
- **Within the ladder,** `or_else` treats every `NoEdge` as settled. The qualified-callee rule ("resolve via the qualifier or not at all", resolver.rs:1650-1680) is `NoEdge(Unresolved)`. That stops the bare-name tiers, as today, and still lets the call-level Kotlin retry and the constructor fallback run, as today.
- **At the `resolve_call` boundary,** `fn call_fallbacks_allowed(&Resolution) -> bool` is the single place that reads `reach()`. Converted sites no longer decide this by hand. They only choose `require` (refused -> `NoEdge(Rejected)`, a ladder reach) or `admit` (refused -> `Unknown`). Both still allow the fallbacks, as today's `return None` / `retain` do.
- **Go `PackageMissing`:** the index returns early (no fallback). The overlay runs `resolve_constructed_type` after it (view.rs:739), but `CallSite::constructed_type` returns `None` for every Go site (types.rs:305-326). So no edge differs today. v2 makes both sides stop, which is the same edge set and needs no fingerprint bump.

The B6/B8 parity claim rests on this table. Their acceptance test adds one fixture per row (§4.3).

### 3.3 Composition

```text
resolve_call(site) =
  normalize_site(site)                      // R2-R5: Bound | NoEdge(Call) | a continuation site
  -> lexical(site)                          // JS/TS: Bound | Shadowed | Unknown
  -> match lexical {
       Shadowed => import_only(site),       // R6 tail: import tiers only (Bound | NoEdge(..) | Unknown)
       Unknown  => resolve_name(lookup),    // ladder; or_else over tiers
       done     => done }
  -> kotlin_retry      if call_fallbacks_allowed && !Bound && typed && Kotlin
  -> admit(member_call_admits)              if untyped
  -> constructor       if call_fallbacks_allowed && !Bound && !may_be_receiverless
```

`resolve_chain` takes `Resolution`. A level's `typed_member` computes `chained_member(..)`, then the chain-level Kotlin retry. A non-`Bound` result after both becomes `NoEdge(ChainBroken)`, except `Unknown` (C1's "several project types" arm and "no return type"), which resolves the level's `fallback()`. A level whose inner result is `NoEdge(ChainBroken)` returns it unchanged. That replaces both `chain_failed` flags with data.

### 3.4 Size and cost

| Type | Size | Heap |
|---|---|---|
| index `Vec<(NodeId, f32)>` (16 signatures in resolver.rs) | 24 B | 1 allocation per bound result (`import_scoped` `vec![..]` resolver.rs:1370) |
| overlay `Option<(u32, f32)>` (16 signatures in view.rs) | 12 B | none |
| `Resolution` | 12 B (const-asserted) | none |

Every tier returns 0 or 1 targets (the cross-check verified this at the 7 push-and-return sites of the ladder and at resolver.rs:1069). B1 adds `debug_assert!(targets.len() <= 1)` in the index adapter.

### 3.5 Sites to convert first

| Site | `ff75103a` | Converted (slice) |
|---|---|---|
| Go, index `resolver.rs:723-747` | `match go_package_lookup { Resolved(t) => return …, Unmapped => site = Plain, Value => site = UntypedMember }` | `normalize_site` -> `GoSelector::Package(Resolution)` / `Value`. `Resolved(None)` -> `NoEdge(PackageMissing)`; `Unmapped` -> `Package(Unknown)`, continue as `Plain(member)` (B2) |
| Go, overlay `view.rs:3591-3620` | same match + hand-set `untyped/free_only/heritage` | the same `normalize_site`; `CallScope` derived from the normalized site (B2) |
| C1, index `resolver.rs:912` (`_ => return None`), `863` | `None` -> `chain_failed` -> stop | **B3 (refactor):** every current `return None` / `?` -> `NoEdge(ChainBroken)` (after the Kotlin retry), so edges are identical. **B3b (behaviour change):** the `_ =>` arm becomes three-way (one project type anywhere -> candidate; none -> `NoEdge(External)`; several -> `Unknown`), plus the positive R-F6 test |
| C1, builder `builder.rs:2610-2647`; overlay `view.rs:583`, `765-800`, `3009-3132` | `chain_failed` flags | deleted (B3) |
| Lexical, builder `builder.rs:2592-2596`; overlay `view.rs:590-605` | `Some(vec![])` -> import-only call | `Resolution::Shadowed` produced by a shared `lexical_step` (B1 defines the state; B8 moves the tail) |

## 4. Migration

### 4.1 Compatibility

| Concern | Answer |
|---|---|
| `graph.bin` schema | B1-B9: no change. B7b adds the crate table (format bump, §1.4). |
| `BUILDER_FINGERPRINT` (`commit_meta.rs:76`) | It embeds `CARGO_PKG_VERSION`, so each release rebuilds anyway (F13); a bump matters only for dev graphs. Refactor slices: no bump. B3b (C1 residual) and B7b change edges and need a bump. **Schema numbers are assigned in merge order across designs A and B.** With the order in §4.2: A-B `schema21`, A-C `schema22`, B3b next free, A-D next, B7b next, A-E last. |
| View cache | Keyed by the binary stamp and git SHA (`view_cache.rs:35-47`); no cleanup. |
| uids | Untouched by B. |
| What an LLM user sees | Refactor slices: nothing. B3b: new `Calls` edges for chained calls whose return type is a unique project type in another directory (C, Go, Rust/PHP without a naming import). B7b: `ecp impact` on a repo with dirty Rust files keeps cross-crate callers. R12 / L1 convergence (B7 / B8, if adopted): the overlay matches a reindex where it misses an edge today. |

### 4.2 Order, interleaved with design A (adopted from the cross-check §2)

Rule: a slice that deletes code goes before the slice that moves it, so a moved copy is never rewritten. Evidence for each dependency, at `ff75103a`:
- **A-C before B5.** A-C deletes the string-path Rust lexical code inside B5's Tier 1 range: resolver.rs `rust_lexical_owner` at 1505 and `prefer_free_scoped` at 1515, inside 1489-1557; view.rs 3671 inside 3652-3720; plus the R32 stores. Otherwise B5 would add `raw_owner` / `rust_lexical_scope` to `ResolveFacts` and A-C would rewrite the moved copy.
- **A-B before B5.** A-B adds `is_lexically_scoped` to the global filter (index.rs:1055-1080 area, view.rs Tier 3), which B5 turns into `global_pick`.
- **A-D before B8.** A-D changes the `LexicalFunctions` input (builder.rs:2277, 2592; view.rs:559-605) that B8 moves.
- **A-E after B8.** It needs the implicit-`this` rule (cross-check F3), which B8 writes once in the shared ladder.
- **A-A / B1 / B2-B4** share no range, but they share files (view.rs, builder.rs), so they run serially (project rule).

| Step | Slice | Depends on | Content (ranges at `ff75103a`) | Est. lines | Behaviour |
|---|---|---|---|---|---|
| 0 | PR 861 final | – | re-pin all ranges (§7) | – | – |
| 1 | **B1** | – | `outcome.rs` with all 4 states and `NoEdge::reach`; `Tier::confidence` replaces `CONF_*` (view.rs:272-278), `MT_CONF` (resolver.rs:1763), the LadderMember maps (resolver.rs:2655, view.rs:3165); R30 helper dedupe; R31 single body; 14-language parity table; the §3.2 fixtures as a test file (they pass before and after every later slice) | ~650 (tests ~250) | none |
| 2 | A-A | B1 (file order only) | design A | A's | A's |
| 3 | **B2** | – | Go selector + `normalize_site` (R2-R5); delete `go_imported_package_member` (reexport.rs:435); `for_each_file_in_directory`, `by_dir` in `LanguageFiles` (import_scope.rs:33-116, 193); dirty-file bench added first | ~600 | none |
| 4 | **B3** | – | chained member (R23, R33) on `Resolution`; `resolve_chain` (types.rs:219); delete both `chain_failed` | ~550 | none |
| 4b | **B3b** | B3 | C1 residual three-way arm + positive cross-directory tests (R-F6) for C, Go, Rust, PHP | ~300 | **changes edges** (bump) |
| 5 | **B4** | – | receiver ladder + `type_candidates` + `csharp_alias_type` (R15, R16) | ~700 | none |
| 6 | A-B | – | design A | A's | A's |
| 7 | A-C | A-B | design A (removes R32 / R7 string-path code) | A's | A's |
| 8 | **B5** | **A-B, A-C** | `resolve_name` skeleton: Tier 1, heritage, global (R7, R18, R20); field reads (R24) and name-ref closures (R25) | ~750 | none |
| 9 | A-D | – | design A | A's | A's |
| 10 | **B6a** | B5 | module discovery: `Module` unifies `IndexedModule` (R9) | ~450 | none |
| 11 | **B6b** | B6a | import-member tier, retry, extension, wildcard (R8, R17 ladder part, R19) | ~600 | none (§3.2 rows) |
| 12 | **B7** | B6b | qualifier tier + module-tree position (R13, R14 order) + R12 decision | ~750 | none, or R12 convergence (overlay only) |
| 12b | **B7b** | B7 | R14 crate table in `graph.bin`; `RustModules` crate heads | ~350 | **overlay gains edges** (format bump) |
| 13 | **B8a** | **A-D**, B7 | call entry + call-level Kotlin retry + post-filter + constructor fallback (R1, R17 call part, R21, R22) | ~600 | none (§3.2 rows) |
| 14 | **B8b** | B8a | lexical tail (R6) as `Shadowed` -> import-only path; L1 decision | ~300 | none, or L1 convergence (overlay only) |
| 15 | **B9** | – | variants, supertypes, namespace owners (R26-R28) | ~700 | none |
| 16 | A-E | **B8a/B8b** | design A | A's | A's |

Changes against the cross-check's list:
- B6 splits into B6a/B6b; it was estimated at ~800 lines.
- B8 splits into B8a/B8b: B8 was the riskiest slice, and the F1/F2 rows now land separately.
- B3b and B7b are separate behaviour-change slices, so B3 and B7 stay pure refactors.

The B total is ~7,300 changed lines, which matches the cross-check's ~7,000 estimate.

### 4.3 Proof per slice

Write the criterion before the run: **the index edge multiset must be identical; one differing edge fails the slice.** For B3b and B7b, the expected new edges are listed in the PR before the run.

1. The overlay parity tests (`overlay_resolver_parity.rs` and the other `overlay_*` tests; harness `tests/common/parity.rs`).
2. **The 14-language parity table (B1).** It compares overlay against reindex per language. Each moving slice extends the fixture of its rule.
3. **The §3.2 fallback fixtures (B1).** There is one fixture per row, and each asserts index and overlay edges:
   - Kotlin typed receiver: ladder `NoEdge(External)` -> extension retry binds.
   - Global ambiguous -> constructor fallback binds a Python class.
   - Qualified miss -> constructor fallback (C++ `Construct`).
   - Go `PackageMissing` -> no edge.
   - Python declared attribute on the import-only path -> no edge.
   - JS lexical `Shadowed` -> named import binds, and the name tiers do not.
   - A chain level `NoEdge(ChainBroken)` -> outer level no edge.

   These fixtures pin today's behaviour before any slice changes the type.
4. **Index edge diff on 4 corpora.** The corpora are `.sample_repo`, this repo, a Go repo with nested `go.mod`, and `.vscode_source`. Compare the old and the new binary, serially. Any differing edge fails the slice.
5. **Overlay self-parity.** Mark K files dirty with unchanged content. The overlay edges must equal the base edges. On this repo this check catches R14 (dirty `resolver.rs` loses its cross-crate calls) until B7b lands. Record that as a baseline file that lists exactly the R14 edge diff, with a pointer to B7b. The check passes only when the diff equals the baseline: any other differing edge fails, and so does a baseline edge that no longer differs. B7b deletes the baseline.
6. **Name what the sample cannot show.** The corpora cover few dirty-file Go selectors through a module-root import, few cross-directory chained returns, and almost no Kotlin typed receivers through an external import. The fixtures in item 3 cover those.

## 5. Performance

### 5.1 Index build path

| Slice | Adds | Removes | Expected |
|---|---|---|---|
| B1 | a `Tier` byte per hit | – | neutral |
| B2 | visitor calls | per-path `Arc<str>` allocation in `IndexFacts::files_in_directory` / `namespace_files` (resolver.rs:185-203) | small gain on Go-heavy repos |
| B3-B8 | one monomorphized ladder per side | the `Vec` allocation per bound reference (gone after B8a) | small gain; layout risk |
| all | – | `record`: no change (runtime option, F11) | – |

Bound: removing one 8-byte allocation per resolved reference cannot speed Pass 2 up by more than allocator time divided by Pass 2 time.

### 5.2 Overlay query path

| Slice | Adds | Removes | Expected |
|---|---|---|---|
| B2 | one hash insert per file in `LanguageFiles::build`; Go namespaces collected once | G4: an O(N files) scan per `files_in_directory` call (import_scope.rs:193-205) and an O(k) membership test per file (reexport.rs:494) | large gain on Go repos with dirty files |
| B5-B8 | the shared ladder may run work the overlay skipped (R12, L1, exact R11 probing) | – | must not regress; the per-tier memo keeps today's keys (§2.2 item 4) |
| B7b | one crate-table read per view | – | negligible |
| — | `RustModules::new` scans all graph files × dirty callers (view.rs:4163-4185), O(N·d) | not changed by B | note only; profile before touching it |

### 5.3 Measurement

- **Index:** three-binary A/B (old, new, control) with `python scripts/benchmark/benchmark_ecp.py` on `.vscode_source` and a Go repo.
- **Overlay:** `benchmark_ecp.py` has no dirty-file scenario. B2 adds one before its code change. It is a release-mode `#[ignore]` timing test for the G4 input (1 dirty Go file, 200 selectors, 15 imports, 15k files), plus an end-to-end `ecp impact` run with a cold view cache and the eprintln timing patch. The target is under 30 ms for 1 dirty file.
- **Verification brief:** delegate bench verification with the bound check: "Removing a piece that takes 10% of the run makes the run at most about 11% faster. A result past the bound measured a cache, a no-op or a bug."

## 6. Cross-check findings and their disposition (design B only)

| Finding | Disposition |
|---|---|
| F1 NoEdge vs fallbacks | **Adopted.** Two reaches; table §3.2; fixtures in B1 |
| F2 lexical fourth outcome | **Adopted.** `Resolution::Shadowed` in B1 |
| F7 new base and new rules | **Adopted.** Base `ff75103a`; R31, R32, R33; R28/G3 status updated |
| F8 file overlap with A | **Adopted.** Interleaved order §4.2 with dependencies |
| F9 R14 cross-crate | **Confirmed** by ecp (§1.1, §1.4); new slice B7b |
| F10 memo semantics | **Adopted.** Per-tier memo with invariants (§2.2 item 4) |
| F11 `record` perf | **Corrected** (§5.1) |
| F13 `schema21` naming | **Adopted.** Numbers assigned in merge order (§4.1) |
| §4 gap G2(b) nested `go.mod` | **Not in B.** It is a config-discovery change (`config_parser.rs:49-64` reads only `<repo>/go.mod`), not a resolution-sharing change. File it as a follow-up; B2's `Package(Unknown)` keeps the fallback for unmapped packages |
| §4 gap G3 | **Closed** at `ff75103a` on both sides (index.rs:499, view.rs:3225-3228) |
| §4 gap R-F6 | **Adopted** in B3b (positive cross-directory test) |
| §4 gap P-F3 | **Not in B.** It is a Python rule defect, not a duplication. B2 moves the rule unchanged; fix it in its own PR |
| §5 "every site still chooses require/admit by hand" | **Narrowed.** Fallback permission now lives in one function (`call_fallbacks_allowed`). Sites choose only `require` vs `admit`, and both permit fallbacks |
| §6 riskiest slice B8 | **Split** into B8a/B8b, after A-D |
| F3-F6, F12, F14, F15 | design A only; no B change |

## 7. Re-pinning ranges

PR 861 may still move, so each slice re-pins its ranges on the final commit. The pins in this document came from a script that runs `git show <sha>:<file>` and greps each anchor (function signature or tier comment). Examples are `// Tier 1: Try SameFile`, `fn resolve_callee`, `let mut chain_failed` and `lexical_lookup.targets`. The implementer reruns it with the new sha; a range is "anchor line to the next anchor".

## 8. Risks, open questions, blind spots

### 8.1 Risks

| Risk | Mitigation |
|---|---|
| A converted site misclassifies a `NoEdge` reach | §3.2 fixtures from B1; `reach()` is a `const fn` table, reviewed once |
| Overlay id semantics leak into shared code | They stay inside the `OverlayFacts` impls; overlay self-parity (§4.3.5) |
| Candidate order (nearest-copy ties) differs | The order is part of the trait doc; a tie test |
| Memo key slices outlive invariant broken | The §2.2 invariant plus a debug assertion that a key's slice pointer lies inside a dirty input or a heritage slice |
| `ResolveFacts` grows until it mirrors `SymbolTable` | Only the methods a moved rule reads are added, per slice |
| PR 861 shape moves | Re-pin per §7 |

### 8.2 Open questions

1. R11: converge the overlay's segment approximation to the exact probe? This needs a corpus count of the sites where the two differ.
2. R12 (Python `super()`): converge in B7 (recommended).
3. L1: confirm with a fixture in B1 (it is one row of the §3.2 table if real).
4. B7b: put the crate table in `graph.bin` (format bump) or in the persisted path aliases (no format bump, but the index would see new aliases too)? Recommended: a separate table, so index edges cannot change.
5. Fold `ResolveFacts` into `TypeFacts`? Kept separate.

### 8.3 Blind spots

- **Nothing compiled or ran.** I used only `git show` / `git grep` / `git diff` and `ECP_SKIP_BG_REBUILD=1 ecp` queries. The ecp queries refreshed session fragments (`l1.refreshed written=2`) but built no index.
- **F1 table:** read from resolver.rs:705-818, 996-1039, builder.rs:2592-2647 and types.rs:305-326. The rows are not run. The overlay side of each row (view.rs:586-763) is read, not traced through every early `return` of `resolve_callee`.
- **R14 / F9 root cause:** taken from `RustModules::new` (view.rs:4150-4200) and `config_parser.rs:66-87`. I did not read `try_module_tree_resolve` / `RustWorkspaceModTree` in full, and did not confirm that the index edge at resolver.rs:1501 came from Tier 3.5 rather than another tier (its confidence is 1.0, which fits SameFile or ModuleTree).
- **Not compared line by line:** R8, R9, R13, R26, R27; their status rests on names, ecp edges, comments and line ranges.
- **Design A's slice contents:** taken from [`2026-10-23-owner-split-design.md`](2026-10-23-owner-split-design.md) and the cross-check. The dependency ranges (A-C at resolver.rs ~1500-1520, A-D at builder.rs 2277/2592) are mapped from `e6f56b8e` to `ff75103a` by anchor, not re-read inside design A.
- **Uncommitted files not read:** 3 files appeared uncommitted again during this revision (`same_file_pick.rs` among them).
- **The 12-byte `Resolution` layout** is not compiled.
