# Design: split `owner_class` into a type owner and a lexical parent (rev 2)

> Archived design record, imported on 2026-10-25. Written against `ff75103a`, a PR #861 commit before its squash merge; fetch it with `git fetch origin pull/861/head`. Line numbers and counts are stale: re-pin them on current `main` before a slice starts. Direction and status: [`2026-10-25-architecture-direction.md`](2026-10-25-architecture-direction.md).

Status: DESIGN, revision 2 (no code, no cargo run). Revision 1 is not kept.
Base: committed HEAD **`ff75103a`** (`fix/session-followups`, PR 861; Codex's working tree is clean). Every line number below is at `ff75103a` (`git show ff75103a:<path>`). PR 861 can still move; re-run the commands in 1.1 on its final commit before slice A.
Revision 2 answers the cross-check [`2026-10-23-architecture-designs-crosscheck.md`](2026-10-23-architecture-designs-crosscheck.md) (F3-F8, F12-F15) and the coordinator's six required changes. Section 11 maps each finding to its answer.

## 0. Decision in one paragraph

`owner_class` keeps one meaning: **the type, namespace or module that declares the node** (plus the Rust uid tags in 1.2, M4). A Function node that a callable body declares (Rust nested `fn`, Rust closure node, Python nested `def`, JS inner function, Kotlin local `fun`) gets **no `owner_class`**. Its lexical scope is a **parent link** to the innermost enclosing callable node, an index, not a string. One pure function, `lexical_parents`, derives the link from the `(kind, span)` list of one file. The parser, the index build, the overlay view, the overlay reader and incremental mode all call it. A scoped node's uid hashes its parent's final uid. Every other node keeps `uid::compute(kind, path, owner_class, name)` unchanged. `graph.bin` stores the links in one sparse sorted table (format v16). Overlay fragments keep their shape; the reader ignores fragments of another builder.

Why not keep `ff75103a`'s approach: `ff75103a` already derives the **caller's** Rust lexical path from spans (`rust_lexical_scopes`, `same_file_pick.rs:545-617`), but the **callee's** path still comes from a separate AST walk (`enclosing_function_path`, `rust/receiver_types.rs:195-249`), and both must produce the same string. They agree by convention (both prefix `Trait::Type` for a trait impl, both skip modules outside an impl), not by construction. Codex has to keep the two walks in step whenever one changes, and R-F2 was the first case where they drifted apart. A parent index has one derivation and no string to compare.

## 1. Inventory at `ff75103a`

### 1.1 Commands and counts

```
$ git grep -l owner_class ff75103a -- 'crates/*.rs' | wc -l                                         -> 177 files
$ git grep -l owner_class ff75103a -- 'crates/*.rs' | grep -v -E '/tests/|/benches/' | wc -l        -> 89 production files
$ git grep -n owner_class ff75103a -- 'crates/*.rs' | grep -v -E '/tests/|/benches/' | wc -l        -> 471 production lines (469 at e6f56b8e)
$ git grep -n 'uid::compute(' ff75103a -- 'crates/*.rs' | grep -v -E '/tests/|/benches/|graph_fixture' | grep -v '//' | wc -l -> 26
$ git grep -n "rust_lexical\|raw_owner\|prefer_free_scoped\|rust_free_global_candidate\|enclosing_function_path" ff75103a -- 'crates/*/src/*.rs' 'crates/*/src/**/*.rs' | wc -l -> 47 (code this design deletes, 9)
$ git log --format='%h %s' e6f56b8e..ff75103a
  ff75103a test: cover Rust impl nested in a local module
  f35f62d8 fix: retain lexical scopes and imported return type bindings
  829ee2fe fix: preserve Go receivers and lexical constant ownership
```

ecp results (all with `ECP_SKIP_BG_REBUILD=1 … --repo <sessfu>`):

```
ecp impact --target stamp_owner_class_by_span --direction upstream -> 13 parse_file callers (C, C#, C++, Dart, Java, JS, Kotlin, PHP, Python, Ruby, Rust, Swift, TS)
ecp impact --target stamp_owner_fn_by_span   -> stamp_js_function_owners; Python parse_file; JS/TS parse_file at d=2
ecp impact --target enclosing_function_path  -> Rust parse_file (2 call sites: parser.rs:351, :425)
ecp impact --target rust_free_global_candidate -> index.rs <anonymous:1073>, <anonymous:1220>, same_file_pick prefer_free_scoped, view.rs resolve_callee
ecp impact --target prefer_free_scoped       -> resolver.rs <anonymous:1501>, view.rs resolve_callee
ecp impact --target rust_lexical_owner       -> resolver.rs <anonymous:1505>, view.rs <anonymous:3671>
ecp impact --target resolve_owner_class      -> find_schema_bindings (4), impact/bfs run_bfs, inspect (2), pattern, symbol_id resolve_candidates
ecp impact --target load_overlay             -> inspect.rs:656 (production), inspect run
ecp impact --target load_overlay_hits        -> 2 test callers only (overlay_view_inputs.rs:484, :541)
ecp impact --target node_uid_for             -> incremental.rs symbol_hash_diff (:92) + 7 unit tests + 2 integration tests
```

ecp does not attribute the field reads of `RawNode.owner_class` (it lists 0 readers; 10.3). The field-read rows below come from `git grep` at `ff75103a`. The last three ecp calls printed `l2.built elapsed=1.10s`; I then stopped using ecp (10.4).

### 1.2 The five meanings

| Id | Meaning | Kinds | Example at `ff75103a` |
|---|---|---|---|
| M1 | Type owner | Method, Constructor, Property, class Const, EnumVariant, nested type | `impl Dog { fn bark }` → `Dog` |
| M2 | Namespace / module owner | Function, Const, types under `mod` / `namespace` | `mod a { mod b { fn f } }` → `a::b` (main: `b`) |
| M3 | Lexical scope | nested Function, Rust closure node, Rust fn-local Const / Macro, Go `:=` Variable, Python / JS non-callables in a function | `Display::A::fmt`, `Cls::m`, `outer` |
| M4 | uid tag only | Rust `Impl` (trait text or `""`), Rust associated `Typedef`, EntryPoint uid | `impl Display for A` → `Display` |
| M5 | Display / FQN | every node, read back from `graph.bin` | `ecp impact --target Foo.validate` |

M3 writers at `ff75103a`:

| Writer | Languages | String | Origin |
|---|---|---|---|
| `rust/receiver_types.rs:195-249` `enclosing_function_path`, called at `rust/parser.rs:425` (Function) and `:351` (closure node) | Rust | `Display::A::fmt`, `T::d`, `a::b`, `outer` | PR 861 (`829ee2fe`/`f35f62d8` added trait prefix, trait segment, closure owner) |
| `rust/receiver_types.rs:168-183` `enclosing_function_name`, `rust/parser.rs:445-450` | Rust Const / Macro | `outer` | main |
| `framework_helpers.rs:736-767` `stamp_js_function_owners` | JS, TS (Function only) | `Cls::m::inner` | main |
| `framework_helpers.rs:421-450` `stamp_owner_fn_by_span` | Python, JS, TS (**every kind** with no owner) | innermost function name | main |
| `go/parser.rs:761-796` | Go `:=` Variable | enclosing function name | main |

A second, transient M3 form exists only in memory: `same_file_pick.rs:545-617` `rust_lexical_scopes` (span-derived caller path), stored in `index.rs:203` and `view.rs:1671`.

### 1.3 Sites by group (production, `ff75103a`)

| Group | Site | Meaning | Change |
|---|---|---|---|
| Schema | `ecp-core/src/analyzer/types.rs:435` `RawNode.owner_class` | all | M3 removed for scoped kinds |
| Schema | `ecp-core/src/graph.rs:702` `Node.owner_class`, docs `:682`, `:16-18` | all | M3 removed; table 4.3 |
| Schema (unrelated) | `types.rs:902` `RawSchemaField.owner_class` | schema model owner | none |
| uid | `ecp-core/src/uid.rs:17` `compute` | M1-M4 | add `scoped`, `assign_file` |
| uid, numbers overloads today | `resolution/builder.rs:1092-1137` Pass 1, `:1165-1181` hint | M1-M4 | via `assign_file` |
| uid, numbers overloads today | `ecp-core/src/session/view.rs:372-387` | M1-M4 | via `assign_file` |
| uid, **numbers no overloads today** | `builder.rs:3009`, `:3046`, `:3107`, `:3141` T7-6 skip sets | M1-M4 | via `assign_file` (behaviour change, slice A) |
| uid, **numbers no overloads today** | `post_process/class_membership.rs:69-74` skip set | M1-M4 | via `assign_file` (slice A) |
| uid, **numbers no overloads today** | `ecp-cli/src/incremental.rs:284-297` `node_uid_for` (used by `symbol_hash_diff` `:92`) | M1-M4 | via `assign_file` (slice A) |
| uid, **numbers no overloads today** | `ecp-cli/src/session/overlay_reader.rs:75-90` `FragSym::new`, for `load_overlay` `:216` (→ `inspect.rs:656`) and `load_overlay_hits` `:199` | M1-M4 | via `assign_file` (slice A) |
| uid | `builder.rs:1741-1746` EntryPoint; Route / File / synthetic sites | M4 / none | none |
| uid | `ecp-core/src/graph.rs:1440-1457` `is_later_overload` | M1 | none; invariant I3 |
| Writers | `framework_helpers.rs:260-351` `stamp_owner_class_by_span` | M1, M2 | none (final clear step, 4.2) |
| Writers | `framework_helpers.rs:421-450` `stamp_owner_fn_by_span` | M3 | slice D: skip kinds in `lexical_scope_kinds` only |
| Writers | `framework_helpers.rs:736-767` `stamp_js_function_owners` | M3 | slice D: deleted |
| Writers | `rust/parser.rs:351`, `:418-457`, `:682`; `rust/receiver_types.rs:185-249` | M1-M4 | slice C: Function arm = `enclosing_impl_type`; closure owner `None`; path walk deleted |
| Writers (other) | `go/parser.rs:761-796`, `python/parser.rs:1482-1483`, `swift/parser.rs:687-735`, `cpp/recovery.rs:587-667`, `cpp/parser.rs:763-911`, `c/parser.rs:798-812`, `php/parser.rs:1079-1102` | M1, M2 (Go: M3) | none (Go: slice F) |
| HasMethod | `class_membership.rs:211-277` Pass 1, `:290-347` Pass 2 | M1 | skip scoped nodes (6.3) |
| Defines | `post_process/scope_defines.rs:85-123` (segment heuristic `:98-111`) | M2 | skip scoped nodes; heuristic deleted |
| Defines | `post_process/enum_variant_defines.rs:69,83,191` | M1 | none |
| Resolver | `resolution/index.rs:196-198` `raw_owner_overrides`, `:634-637` | M3 | deleted |
| Resolver | `index.rs:199-203` `rust_lexical_scopes`, `:736-748` `note_rust_lexical_scopes`, `builder.rs:1266` | M3 | replaced by `lexical_parent` map |
| Resolver | `index.rs:1539-1548` `raw_owner`, `rust_lexical_scope` | M3 | deleted |
| Resolver | `index.rs:1055` `lookup_global` filter `:1074`; `:1205` `nearest_global_copy` filter `:1221` | M3 | `!is_lexically_scoped(id)` |
| Resolver | `index.rs:890` `lookup_call_in_file`; `:1510` `pick_candidates` | M1 | add `!is_lexically_scoped(id)` (slice B, no-op while empty) |
| Resolver | `resolver.rs:1499-1530` caller lexical owner + `prefer_free_scoped` | M3 | deleted; lexical tier + `prefer_free` |
| Resolver | `builder.rs:2277-2291` `lexical_functions`; `:2592` lexical tier | M3 | input = `lexical_parents` + admission (4.2) |
| Resolver | `builder.rs:2363`, `view.rs:4674` Rust callable subject; `builder.rs:2459`, `view.rs:4546` `Self` | M1 | none |
| Shared rules | `same_file_pick.rs:64-76` `raw_owner`, `rust_lexical_scope` | M3 | deleted |
| Shared rules | `same_file_pick.rs:99-114` `rust_lexical_owner`; `:143-160` `rust_free_global_candidate`; `:207-248` `prefer_free_scoped`; `:537-617` `rust_lexical_scopes` | M3 | deleted |
| Shared rules | `same_file_pick.rs:162-205` `prefer_free`; `:138-141` `is_free` | M1, M2 | `is_free` adds `!is_lexically_scoped` |
| Shared rules | `analyzer/lexical.rs:21-58` `LexicalFunctions::new` (gate `:29`, admission `:53`), `:70-95` `targets` | M3 | admission per language (4.2) |
| Overlay | `view.rs:559` LexicalFunctions input; `:1671`, `:1713`, `:1755-1757`, `:1818` dirty lexical scopes | M3 | `lexical_parents` |
| Overlay | `view.rs:3199-3215` `raw_owner`, `rust_lexical_scope` | M3 | deleted |
| Overlay | `view.rs:2471` `decls_in_file`; `:3176` `pick_candidates`; `:3629-3638` global-tier `admits` | M1, M3 | `!is_lexically_scoped` |
| Overlay | `view.rs:3668-3685` Rust caller lexical owner | M3 | deleted |
| Overlay | `view.rs:2023` `owner_of` and the other M1 reads | M1, M2 | none |
| Overlay files | `overlay_writer.rs:85-96`, `:183` | M1-M4 | none |
| Overlay files | `overlay_reader.rs:98-194` `for_each_fragment` (no fingerprint gate), `:216` `load_overlay` | M1-M4 | gate in slice C (5.4) |
| CLI | `commands/symbol_id.rs:13-22`, `:122-129`, `:155-159` | M5 | `display_owner` |
| CLI | `commands/rename.rs:314-341` | M5 | bare name excludes scoped nodes |
| CLI | `commands/diff/symbols.rs:45-53` `SymbolKey`, `:237-246`; `diff/output.rs:177-189`; `review/verdicts.rs:222-446` | M5 | `display_owner` + `ordinal` in the key (F12) |
| CLI | `impact/bfs.rs:408,428,438`; `inspect.rs:228,285,355-359,580`; `pattern.rs:289`; `cypher/executor.rs:1837,2496`; `session/merged.rs:313-319` | M5 | `display_owner` |
| CLI (M1 only) | `find_tx_patterns.rs:111,277`; `find_schema_bindings.rs:134,169,207,238`; `saga_pairs.rs:113`; `php/laravel_routes.rs:1714` | M1 | none |
| Dev | `commands/dev/uid_audit.rs:193,280,322-328` | all | print `display_owner` |

`PickFacts::namespace_visible_from` (`same_file_pick.rs:83-89`, 3 impls: `index.rs:1566`, `view.rs:3264`, `import_scope.rs:157`) does not read `owner_class`. This design leaves it alone. The overlay-sharing design (B) must absorb it (cross-check F7).

## 2. Defects removed

| Id | Input | At `ff75103a` | After |
|---|---|---|---|
| R-F1 | `impl Display for A { fn fmt { fn pad } }` + `impl Debug for A { fn fmt { fn pad } }` | fixed by string `Display::A::fmt` vs `Debug::A::fmt` (two derivations) | `fmt#1` uid `U`, `fmt#2` `overload(U,1)`; `pad#k = scoped(fmt#k)`; the lexical tier binds the `pad` whose parent span contains the call |
| R-F2 | trait default method / impl in a fn | fixed by two string derivations that must agree | one derivation; no string |
| R-F3 | call from a closure inside `outer` | fixed for Rust (closure owner = path) | closure node is scoped to `outer`; the lexical tier tests span containment |
| R-F4 | `mod a { pub mod b { pub fn f(){} } }` | `a::b` plus two segment heuristics (`scope_defines.rs:98-111`, `same_file_pick.rs:155-157`) | `f` is unscoped; owner `b` (main's uid); heuristics deleted |
| new | Python `class A: def m(self): def h()` | `h` owner `A`; fabricated `A -HasMethod-> h`; two methods with `h` collide | `h` scoped; no HasMethod |
| new | Kotlin / Swift / C# / Dart `fun outer(){ fun h(){} }` + top-level `h` | same uid, one tombstoned | two nodes |

## 3. Rules

### 3.1 Scoped node

Node `N` of language `L` is scoped when all hold:

| # | Condition |
|---|---|
| S1 | `N.kind ∈ lexical_scope_kinds(L)` (5.2; `Function` only in this program) |
| S2 | `!uid::keeps_overloads(N.kind)` |
| S3 | The innermost container that strictly contains `N.span` is a callable |

Containers: `Function`, `Method`, `Constructor`, `Class`, `Struct`, `Enum`, `Interface`, `Trait`, `Impl`, `Namespace`, `Module`. "Innermost" and "strictly" follow `analyzer::types::innermost_enclosing` (`types.rs:40-63`).

### 3.2 Invariants

| Id | Invariant |
|---|---|
| I1 | A scoped node has `owner_class == None` when `parse_file` returns. |
| I2 | Every consumer derives parents with `lexical_parents` from `(kind, span)` only. |
| I3 | `keeps_overloads(kind)` ⇒ never scoped (keeps `is_later_overload` correct). |
| I4 | An unscoped node's uid is `compute(kind, path, owner_class, name)`; a later overload's is `overload(first, ordinal)`, in **every** path (fixes the 4 paths that skip ordinals today). |
| I5 | A scoped node is reachable only through the lexical tier. Every name-tier candidate source filters it out. |

## 4. API

### 4.1 uid (`ecp-core/src/uid.rs`)

```rust
/// UID of a scoped node. The tag byte 0x01 never starts `NodeKind::as_str`
/// (ASCII letters), so this stream never equals a `compute` stream.
pub fn scoped(parent: u64, kind: NodeKind, name: &str) -> u64;

pub struct UidInput<'a> {
    pub kind: NodeKind,
    pub owner: Option<&'a str>,
    pub name: &'a str,
    pub parent: Option<u32>, // index into the same slice, from `lexical_parents`
}

pub enum UidSlot {
    First(u64),
    Overload { first: u32, ordinal: u32, uid: u64 },
    Duplicate { first: u32, uid: u64 },
}

/// Every uid of one file's declarations, in slice order: unscoped nodes in
/// order with overload ordinals (builder.rs:1119-1137 rule), then scoped
/// nodes through their parent's final uid (memoised; no order requirement).
pub fn assign_file(path: &str, nodes: &[UidInput<'_>]) -> Vec<UidSlot>;
```

| Case | uid |
|---|---|
| unscoped, first of identity | `compute(kind, path, owner, name)` |
| unscoped later, `keeps_overloads` | `overload(first, ordinal)` |
| unscoped later, other kind | `Duplicate` (builder tombstones it, as today) |
| scoped | `scoped(final_uid(parent), kind, name)`; repeated → `Duplicate` |
| parent is `Duplicate` | parent's identity uid (the twin owns it); children then collide, as today |

**No-collision argument.** (a) Unscoped nodes hash exactly today's inputs, so their uid set and collision set are today's. (b) A scoped stream starts with byte 0x01 and an unscoped stream with an ASCII letter, so the two can only meet by a 64-bit hash collision, the same residual risk as today. (c) Two scoped nodes share a stream only when parent uid, kind and name are equal: that is a redeclaration in one scope, which today also collides. Parent uids are distinct per surviving node because (a) and I4 give every surviving node a distinct uid.

### 4.2 Lexical parents and the lexical tier (`ecp-core/src/analyzer/lexical.rs`)

```rust
pub fn lexical_scope_kinds(language: Language) -> &'static [NodeKind];
pub fn lexical_parents(language: Language, decls: impl ExactSizeIterator<Item = (NodeKind, Span)>) -> Vec<Option<u32>>;

/// Run once at the end of every `parse_file` of a language with a non-empty
/// `lexical_scope_kinds`: sets `owner_class = None` on scoped nodes (I1).
/// Parsers and stamps stay as they are; this one step overrides them.
pub fn clear_scoped_owners(language: Language, nodes: &mut [RawNode]);

/// Which unscoped functions the lexical tier admits.
pub enum LexicalAdmission {
    /// Today's JS / TS rule (`lexical.rs:53`): scoped functions, plus every
    /// file-level function without an owner, with scope = whole file.
    ScopedAndFileLevel,
    /// Only scoped functions. Unscoped functions stay with the name tiers.
    ScopedOnly,
}
pub fn lexical_admission(language: Language) -> LexicalAdmission;
```

`LexicalFunctions::new` takes `(kind, name, parent: Option<u32>, has_owner, span)` and an admission. The extension gate (`lexical.rs:29`) becomes `!lexical_scope_kinds(language).is_empty()`.

`targets(caller, callee)` per admission:

| Situation | `ScopedAndFileLevel` (JS, TS) | `ScopedOnly` (Rust, Python, Kotlin, Swift, C#, Dart) |
|---|---|---|
| no candidate of that name | `None` | `None` |
| one innermost visible scope, one candidate | `Some([t])` | `Some([t])` |
| innermost visible scope has two candidates | `Some([])` (import tiers only) | `Some([])` |
| candidates exist but none is visible from the caller | `Some([])` (today) | **`None`**: the name tiers decide, and I5 hides the invisible scoped nodes from them |

Why `ScopedOnly` (cross-check F3):

- Rust `fn helper(){}` + `mod tests { fn helper(){} fn t(){ helper(); } }`: `tests::helper` is unscoped (container `Module tests`), so the lexical tier returns `None`. `prefer_free` (`same_file_pick.rs:171-203`) then prefers the `tests` member, as today.
- Kotlin / Swift / C# / Dart `class C { fun helper(){}; fun m(){ helper() } }` + top-level `fun helper()`: the top-level function is not a lexical candidate. The implicit-`this` name tier decides, as today. A local `fun helper()` inside `m` is scoped and wins (Kotlin, Swift, C# and Dart all resolve a local function before a member).
- JS / TS keep `ScopedAndFileLevel`; their lexical tests stay green unchanged.

I5 filter sites (added in slice B; no-op while no language opts in): `index.rs:890`, `:1510`, `:1074`, `:1221`; `view.rs:2471`, `:3176`, `:3629-3638`; `is_free` (`same_file_pick.rs:138-141`). `PickFacts` gains:

```rust
fn is_lexically_scoped(&self, id: u32) -> bool; // index: sparse map; view: dirty map + graph table
```

Interaction with the outcome type of design B (cross-check F2): `targets` keeps its three outcomes. B1 maps `Some([])` to its import-only state. This design adds no fourth outcome.

### 4.3 `graph.bin` v16

```rust
pub struct LexicalParentFact { pub node_idx: u32, pub parent_idx: u32 }
// ZeroCopyGraph, after `route_middleware` (graph.rs:1234):
/// Sparse, sorted by `node_idx`: one entry per scoped node.
pub lexical_parents: Vec<LexicalParentFact>,
impl ArchivedZeroCopyGraph { pub fn lexical_parent(&self, node_idx: u32) -> Option<u32>; }
```

| Reader | Needs | Without a table |
|---|---|---|
| overlay global tier and `is_lexically_scoped` for a clean base node | bit | recompute the plain uid and compare (I4 makes that exact), 0 bytes |
| `display_owner`, `rename`, diff identity, FQN targets | parent | span scan over the file's nodes; `graph.bin` has no per-file node range |

Cost: 8 B per scoped node; the root grows by one vector header and stays within `GRAPH_ROOT_TAIL_BYTES = 4096` (cross-check §3). The builder maps a tombstoned parent to its surviving twin.

### 4.4 Overlay fragments

Shape unchanged (`FragmentSymbol` has kind, span and owner for every node, in `LocalGraph.nodes` order). Two read paths exist:

| Path | Fingerprint gate at `ff75103a` | After slice C |
|---|---|---|
| `accepted_fragments` (`overlay_reader.rs:274-313`) → view | `from_current_builder()` at `:284` | unchanged |
| `load_overlay` (`:216`) → `ecp inspect` (`inspect.rs:656`) | **none** (`for_each_fragment` `:98`) | gate added: a manifest of another builder gives `None` (no overlay) |
| `load_overlay_hits` (`:199`), test callers only | none | unchanged; documented: uids of another builder's bins are not comparable. Option for the user: delete it (no production caller) |

The gate makes `ecp inspect` treat a stale session as clean until the next `ensure_fresh` re-parses it. `accepted_fragments` already behaves this way.

### 4.5 Display owner

```rust
/// `owner_class` when set; for a scoped node, `display_owner(parent)::parent.name`
/// (or `parent.name` when the parent has none); else `None`.
pub fn display_owner<'g>(graph: &'g ArchivedZeroCopyGraph, idx: u32) -> Option<Cow<'g, str>>;
```

| Node | stored `owner_class` | printed | vs today |
|---|---|---|---|
| Rust nested `helper` in `impl A { fn new }` | none | `A::new` | `ff75103a` prints `A::new` |
| Rust nested `pad` in `impl Display for A { fn fmt }` | none | `A::fmt` | `ff75103a` prints `Display::A::fmt` (Q2) |
| JS `inner` in `class C { m(){} }` | none | `C::m` | same |
| Rust `mod a { mod b { fn f } }` | `b` | `b` | `ff75103a`: `a::b`; main: `b` |
| Python `h` in `A.m` | none | `A::m` | today `A` |

`SymbolKey` (diff) becomes `{path, display_owner, name, kind, ordinal}`, where `ordinal` counts earlier nodes with the same first four fields in node order. Two R-F1 `pad` nodes and two overloads then both show (F12). This is a deliberate output change of slice B, with a test.

## 5. Migration

### 5.1 Versions

`BUILDER_FINGERPRINT = concat!("v", CARGO_PKG_VERSION, "+schemaN")` (`commit_meta.rs:76`), so every release already rebuilds. A bump inside a release matters for dev builds and for session manifests. Take the **next free `schemaN` at merge time**; design B may claim numbers in between (F13).

| Slice | graph format | fingerprint | fragment format |
|---|---|---|---|
| A | 15 | bump (incremental and overlay uids of later overloads change) | V5 |
| B | 15 → 16 | bump | V5 |
| C | 16 | bump | V5 (gate added) |
| D | 16 | bump | V5 |
| E | 16 | bump | V5 |

### 5.2 Languages

| Language | Nested named callable | Owner today (source-read) | Opt in | Admission | Slice |
|---|---|---|---|---|---|
| Rust | nested `fn`, closure node | lexical path | `Function` | ScopedOnly | C |
| Python | nested `def` | class (in a method) or innermost fn name | `Function` | ScopedOnly | D |
| JavaScript, TypeScript | inner function | full path | `Function` | ScopedAndFileLevel | D |
| Kotlin, Swift, C#, Dart | local function | class or none | `Function` (after capture check, Q1) | ScopedOnly | E |
| Go | none named; `x := func(){}` is a Variable | Variable: fn name | none now | — | F |
| PHP | nested `function` (global at runtime) | — | none | — | — |
| Ruby | `def` in `def` (method on the class) | — | none | — | — |
| Java, C, C++ | none named | — | none | — | — |

**Non-callables stay as they are** (cross-check F4). `stamp_owner_fn_by_span` (`framework_helpers.rs:421-450`) stamps the innermost function name on every unowned kind; Python's unanchored `class_definition` query emits classes in function bodies. Slice D keeps the function and adds one filter: skip `lexical_scope_kinds(language)`. Local classes, Variables and Consts keep `owner = function name` and their uids. Proof: rows (a)-(c) in 4.1. The scoped Function nodes leave the `compute` space, and the non-callables keep exactly today's inputs. Slice D asserts it: `test_parse_python_local_class_in_two_functions_keeps_two_uids` (two test functions, each `class Fake`; no `uid-collision` BlindSpot; uids equal to the pre-slice binary).

### 5.3 Uid stability

| Node | uid |
|---|---|
| unscoped (top-level fn, method, type, property, module member, Impl, Typedef, EntryPoint, Route, File, local class) | unchanged in full builds |
| later overload in overlay reader / incremental | now equal to the full build's (fix, slice A) |
| Rust `mod a { mod b { fn f } }` | back to main's (`b`) |
| scoped | new |

Check per opt-in slice (main session): dump `(uid, kind, path, display_owner, name, start_line)` of `.sample_repo` and the ecp repo with the old and new binary. Criterion written before the run: every changed uid belongs to a node that `lexical_parents` scopes, and every other uid is equal.

### 5.4 Old data

| Item | Behaviour | Test (data written by the **previous** binary) |
|---|---|---|
| `graph.bin` v15 | refused; rebuilt | slice B: v15 file from the slice-A binary |
| session manifest + fragments of another builder | view: skipped (`:284`). `ecp inspect`: skipped after the slice-C gate | slice C: session dir written by the `ff75103a`/PR 861 binary with the R-F1 file dirty; assert `load_overlay` returns `None` and `ecp inspect fmt` lists base nodes only; then `ensure_fresh` re-parses and `inspect` shows two `pad`s. Pass/fail before the run: the bug lists `pad` with owner `Display::A::fmt` from the stale fragment; the fix lists none until re-parse |
| `parse_cache/<fp>/`, L2 generations, commit dirs | swept with 7-day grace (`gc.rs:43`, `:384`, `:426`, `:611`) | `ecp admin gc --dry-run` after the grace lists the old `<fp>` |

What the slice-C fixture cannot show: three or more impls (ordinal ≥ 2), and positional swap (F14). Add a second fixture with three impls.

### 5.5 LLM-visible changes

| Command | Change |
|---|---|
| `ecp find` / `inspect` | `ownerClass` prints `display_owner`; Rust nested-in-trait-impl prints `A::fmt` instead of `Display::A::fmt` (Q2) |
| `ecp impact --target b.f` | works for nested modules again |
| `ecp inspect` with a stale session | stale fragments ignored until re-parse (slice C) |
| `ecp inspect` / incremental with overloads | later overloads get their own uid (slice A) |
| `ecp diff` / `review` | two same-named nodes (overloads, R-F1 `pad`) both listed (slice B) |
| `ecp inspect <class>` | Python / Kotlin / Swift / C# / Dart: no nested function listed as `HasMethod` |
| `ecp rename helper` | never renames a scoped `helper` |

## 6. Slices and order

### 6.1 Order: adopt the cross-check's interleaving

Adopted. Rule: a slice that deletes code goes before the slice that moves it. A slice that only adds a shared type goes first.

| Step | Slice | Why here |
|---|---|---|
| 0 | PR 861 final | re-run 1.1 |
| 1 | B1 (design B outcome type) | touches no contested range |
| 2 | **A** uid unification | disjoint from B; serial after B1 (shared files view.rs / builder.rs) |
| 3-5 | B2, B3, B4 | independent of this design |
| 6 | **B** table + `display_owner` + I5 filter sites | before B5, so B5's global-pick contract includes the filter |
| 7 | **C0** 14-language lexical-scope parity tests (tests only) | splits the tests out of C (F15) |
| 8 | **C** Rust opt-in | deletes the Rust string-path code before B5 moves Tier 1 |
| 9 | B5 | moves the simplified Tier 1 |
| 10 | **D** Python, JS, TS | before B8 moves the lexical tail |
| 11-14 | B6, B7, B8, B9 | B8 models the final lexical semantics once |
| 15 | **E** Kotlin, Swift, C#, Dart | needs ScopedOnly and the implicit-this tests |

One amendment: B8 must keep `LexicalAdmission` as an input of the lexical tail, not re-derive it from the file extension.

### 6.2 Slice contents

Every slice: sub-agents edit only; the main session runs cargo serially, one crate, `-j 4`.

**A — uid unification (a fix, not a pure refactor; ~450 lines).**
- Add `uid::scoped`, `UidInput`, `UidSlot`, `assign_file`; `lexical_parents` / `lexical_scope_kinds` return empty results.
- Route the 9 file-level uid sites of 1.3 through `assign_file`.
- Behaviour change, stated: four paths now number later overloads. (1) Incremental (`incremental.rs:284`, builder skip sets `:3009-3141`, `class_membership.rs:69`). Today a later overload hashes to its first overload's identity: it is compared against the first overload's old body hash and is usually marked changed, and the shared identity then also un-skips the first. After A, each overload compares with its own old hash. Expected effect: fewer re-resolved symbols and the same edges. (2) `load_overlay` (`overlay_reader.rs:83`). Today a dirty file's later overload carries the first's uid in `ecp inspect`. After A it carries its own and merges with its own base twin.
- Tests, 14 languages (`crates/ecp-cli/tests/uid_assign_parity.rs`, one table-driven case per language: TS, JS, Python, Java, Kotlin, C#, Go, Rust, PHP, Ruby, Swift, C, C++, Dart). Each fixture holds overloads where the language has them (Java, Kotlin, C#, C++, Swift, Dart, TS signatures, Rust two impl blocks). Each case asserts **four paths give equal uids per node**: full build (`graph.bin`), `OverlayView` (`view.rs:372`), `load_overlay` (`inspect` path), and `node_uid_for` / skip-set uids.
- Incremental parity test: full build, edit one overload's body, incremental rebuild. Assert the edge multiset equals a fresh full build, and `skipped_count` includes the unchanged overload (fails before A).
- Overlay parity: dirty a Java file with `save(int)` / `save(String)`; `ecp inspect save` lists two nodes, each `replaced_base` to its own twin.
- Full-build parity: `.sample_repo` and ecp repo dump byte-identical to the pre-A binary.

**B — table, display owner, I5 sites (table empty; ~550 lines).**
- `LexicalParentFact`, v16, `lexical_parent()`, `display_owner()`; M5 sites → `display_owner`; `rename` bare-name filter; `SymbolKey.ordinal`; `is_lexically_scoped` and the I5 filters.
- Parity: CLI output of `find` / `inspect` / `impact` / `cypher` / `review` on `.sample_repo` equal to slice A, except `diff` / `review` listing a second same-named node (test: Java overloads changed in a diff both appear).
- Stored data: v15 graph from the slice-A binary is refused and rebuilt.

**C0 — lexical-scope tests (tests only; ~700 lines across 14 files).** `crates/ecp-analyzer/tests/<lang>_lexical_scope.rs` for the 14 languages. Each pins today's nested-function uids and edges, including the admission cases below. All pass on the C0 commit. Later slices flip their language's assertions in the same PR that opts the language in, so each opt-in diff shows exactly the changed expectations.

**C — Rust opt-in (~450 lines, mostly deletions).**
- `lexical_scope_kinds(Rust) = [Function]`, `ScopedOnly`; `clear_scoped_owners` at the end of Rust `parse_file`; parser Function arm = `enclosing_impl_type`; closure owner `None` (`parser.rs:351`).
- Delete the code of section 9. `scope_defines` and `class_membership` skip scoped nodes. The `load_overlay` fingerprint gate (4.4).
- Tests (flip `rust_lexical_scope.rs`): R-F1 (2 and 3 impls), R-F2 (trait default; impl in fn; impl in local module, the `ff75103a` test), R-F3 closure, R-F4 two-level `mod` (Defines, global tier, `--target b.f`), **`mod tests { fn helper(){} fn t(){ helper(); } }` + file-level `helper` → binds `tests::helper`**, nested `helper` in `a` not reachable from `b` (binds the top-level one), rename skips nested, overlay parity of each, stale-session fixture (5.4).

**D — Python, JS, TS (~400 lines).**
- Opt in; Python `ScopedOnly`, JS/TS `ScopedAndFileLevel`. `stamp_owner_fn_by_span` skips scoped kinds; delete the Function half of `stamp_js_function_owners` (`:738-766`); `LexicalFunctions` consumes `lexical_parents`.
- Tests: Python nested `def` in two methods = two nodes, no HasMethod; Python local class in two functions keeps uids (5.2); **Python method calling bare `helper()` with a module-level `helper` and a nested `helper` in another method → binds the module-level one**; JS/TS existing lexical tests unchanged.

**E — Kotlin, Swift, C#, Dart (~400 lines).**
- Verify the capture per grammar (Q1); opt in what is captured; `ScopedOnly`.
- Tests per language: **implicit-this member `helper()` with a top-level `helper` binds the member (unchanged)**; local `helper` inside the method binds the local one; local `h` + top-level `h` are two nodes; no HasMethod to a local function.

**F — follow-ups (file as FU).** Go `:=` Variables, Rust fn-local Const / Macro, local types (P-F6 class), optional `Function -Defines-> scoped node`.

## 7. Performance

| Slice | Index build | Overlay query (view-cache miss) |
|---|---|---|
| A | one `Vec<UidSlot>` per file (24 B × n, freed after Pass 1); incremental skips more | one `assign_file` per dirty file (already one identity pass) |
| B | empty table write; I5 filter = one hash probe per candidate on an empty map | one binary search per global candidate |
| C | removes per-Function AST path walk with `String` joins (`enclosing_function_path`), `rust_lexical_scopes` `format!` per scope node, `raw_owner_overrides`, `rust_lexical_scopes` map, and one `format!` per Rust receiverless call (`resolver.rs:1505`); adds one `innermost_enclosing` sort per Rust file | removes `dirty_rust_lexical_scopes` strings and the per-call `format!` (`view.rs:3671`); adds one sort per dirty Rust file |
| D | removes the JS/TS path strings and one of two span sweeps; Python adds one sort | same |
| E | one sort per file of four languages | same |

Measurement after A, C and D (main session, serial): `python scripts/benchmark/benchmark_ecp.py` index build on the ecp repo and `.vscode_source`; `ecp impact` with 5 dirty Rust files, view cache cleared, 30 runs, median and p95. Three binaries: old, new, control (new with `assign_file` / `lexical_parents` replaced by byte-identical copies of the old code). Bound sentence for the verifier: "Removing a piece that takes 10% of the run makes the run at most about 11% faster. A result past the bound measured a cache, a no-op or a bug."

## 8. Risks and open questions

| Id | Item | Proposal |
|---|---|---|
| Q1 | Kotlin / Swift / Dart local-function capture unverified | check per grammar at the start of E |
| Q2 | R-F1 `pad` prints `A::fmt` (ambiguous; `ff75103a` prints `Display::A::fmt`) | user decision: keep plain (file:line disambiguates) or print the trait for a trait-impl parent |
| Q3 | `fn outer(){ mod a { fn f(){} } }`: `f` unscoped, global tier admits it | pre-existing; FU |
| Q4 | `scope_defines.rs:80` finds the child by name, so `File -Defines->` can land on a same-named node | fix in C with node ids (small, local) |
| Q5 | macro-generated Rust fns have no nested span | stay unscoped |
| Q6 | `name_ref.rs` References (R-F5) use owner text | C routes `resolve_name_ref` bare names through `targets` too, else FU |
| Q7 | PR 861 may move after `ff75103a` | re-run 1.1 before A |
| Q8 | `load_overlay_hits` has no production caller | ask: delete (Minor) |
| R1 | a reader treats `owner_class == None` as top-level | B audits `git grep -n "owner_class.is_none()\|owner_class.len"` and adds the scoped check |
| R2 | F14: ordinals are positional; reordering impls swaps `fmt#1` / `fmt#2` and every scoped child uid | pre-existing for methods; accepted and documented; the overlay's `replaced_base` maps by position, as for overloads today |

## 9. Code this design deletes (at `ff75103a`)

| File | Lines | Item |
|---|---|---|
| `rust/receiver_types.rs` | 185-249 | `enclosing_function_path` |
| `rust/parser.rs` | 351, 425 | its two calls |
| `same_file_pick.rs` | 64-76 | `raw_owner`, `rust_lexical_scope` defaults |
| `same_file_pick.rs` | 99-114 | `rust_lexical_owner` |
| `same_file_pick.rs` | 143-160 | `rust_free_global_candidate` (with segment heuristic) |
| `same_file_pick.rs` | 207-248 | `prefer_free_scoped` |
| `same_file_pick.rs` | 537-617 | `rust_lexical_scopes` |
| `index.rs` | 196-203, 634-637, 736-748, 1539-1548 | `raw_owner_overrides`, `rust_lexical_scopes`, `note_rust_lexical_scopes`, `raw_owner`, `rust_lexical_scope` |
| `builder.rs` | 1266 | `note_rust_lexical_scopes` call |
| `resolver.rs` | 1499-1530 (Rust arm) | lexical owner + `prefer_free_scoped` call |
| `view.rs` | 1671, 1713, 1755-1757, 1818, 3199-3215, 3668-3685 | dirty lexical scopes, `raw_owner`, `rust_lexical_scope`, Rust caller arm |
| `scope_defines.rs` | 98-111 | Rust segment heuristic |
| `framework_helpers.rs` | 738-766 (slice D) | JS path half of `stamp_js_function_owners` |

## 10. Blind spots

1. Nothing ran: no cargo, test, benchmark or index build. Runtime claims are inferred from source.
2. Not read at `ff75103a`: `view.rs` outside the cited ranges, `lookup_call_in_file` body (`index.rs:890`), `decls_in_file` body (`view.rs:2471`), `name_ref.rs`, Kotlin / Swift / Dart / C# query files and their implicit-this resolution arm, `merged.rs`, cypher property code. The I5 filter list may miss a name-tier source.
3. ecp anomaly: field reads of `RawNode.owner_class` are not attributed (0 readers vs hundreds in git grep).
4. ecp printed `l2.built elapsed=0.98s` (revision 1) and `l2.built elapsed=1.10s` (revision 2, after HEAD moved to `ff75103a`). Under the rule, revision 2 stopped using ecp after that call; the 1.1 ecp lines for `load_overlay`, `load_overlay_hits` and `node_uid_for` come from that last run, the others from revision 1 against the working tree that became `ff75103a`. Later checks used `git show` / `git grep` only.
5. The agreement of `enclosing_function_path` and `rust_lexical_scopes` for every input (section 0) is argued, not tested; I did not read how the Rust parser names an `Impl` node with a generic self type.
6. The "fewer re-resolved symbols, same edges" effect of slice A is inferred from `symbol_hash_diff` (`incremental.rs:92-200`) and `build_old_hash_map` (`:218-224`).
7. Line estimates per slice are guesses.

## 11. Cross-check findings and answers

| Finding | Answer |
|---|---|
| F3 lexical tier admits file-level functions | `LexicalAdmission::ScopedOnly` for every newly opted-in language; tests in C, D, E (6.2) |
| F4 `stamp_owner_fn_by_span` stamps every kind | D keeps it for non-callables; no-collision proof 4.1; test 5.2 |
| F5 old fragments are read by `load_overlay` / `inspect` | gate in C; stale-session test 5.4 |
| F6 slice A is not a pure refactor | relabelled a fix; behaviour stated; 14-language four-path uid parity + incremental + overlay tests |
| F7 HEAD moved | rebased on `ff75103a`; 1.3 and 9 re-inventoried; `namespace_visible_from` noted, untouched |
| F8 file overlap with design B | interleaved order adopted (6.1) |
| F12 diff keeps one of two `pad`s | `SymbolKey.ordinal` in B |
| F13 fingerprint numbering | next free `schemaN` at merge time |
| F14 positional ordinals | accepted, R2 |
| F15 C over budget | tests split into C0 |
| F1, F2, F9, F10, F11 | design B findings; F2's import-only state is preserved here (4.2) |
