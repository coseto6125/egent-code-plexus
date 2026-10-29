# Phase 3 slice T2: shared parse driver design

Status: design, not implemented. Slice plan: `docs/plans/2026-10-26-phase3-area10-slices.md`. Target: `docs/specs/2026-10-25-architecture-direction.md` §10. Phase contract: `docs/specs/contracts/C4-raw-records.md` (Area 10).

Every `file:line` below is valid at commit `87f2d860` of branch `feat/arch-p3-parse-driver`, with T1 committed. Paths without a crate prefix are under `crates/ecp-analyzer/src/`.

## Terms

| Term | Meaning |
|---|---|
| driver | The shared code in `driver/` that runs one file's parse. |
| spec | One language's `impl ParseSpec`. It supplies the data and the code that only that language has. |
| step | One entry of a spec's ordered `STEPS` list. Each step runs under one `parse_prof::Pass`. |
| hook | A step whose code the spec supplies. |
| role | A capture that the spec handles in its own `capture` code. |
| tap | A driver table that collects capture ranges for a record family (event topics, schema fields) during the main query pass. |
| tap row | One kept candidate of one config in one tapped match. |
| node-changing step | A step that may push, remove, reorder or retag a node, or write any `RawNode` field. |
| bind step | A step that writes a raw index (today: `RawTxScope`). |
| neutral step | A step that reads nodes and writes only its own record field. |

## Summary

1. Record order must stay as it is. Not every permutation changes every uid or lookup, but each reader in §1 changes the output for some permutation. The dump stays order-sensitive.
2. The driver owns the tree parse, the one query loop, the capture-index table, the kind captures, anonymous callbacks, the unconditional blind-spot captures, the record taps, the per-file record buffers, test classification, the fixed steps and the `LocalGraph` assembly.
3. The spec owns the roles (conditional blind spots included), the per-match emit code, the per-language hooks and the parameters where two copies differ.
4. Each spec lists its steps in its current order. A `const` check rejects a node-changing step after a bind step.
5. Only three languages bind before their node list is final: Python, Java, Kotlin. Their move changes no output.
6. The driver is generic over the spec. The hot loop has no `dyn` call. The only new buffer is the tap-row buffer of §2.5, with the bound stated there.
7. T2 lands the record tap in its own commit after the pure Go and Rust moves. The commit stays only if it passes the perf rule of §5.3.
8. Two pre-query scans stay as approved exceptions (§2.7): Python's import pre-scan and C++'s override pre-scan. The one-query target of the slice plan is not complete while they remain.

## 1. Does record order matter?

Yes. The order of `LocalGraph.nodes`, of `imports` and of the side tables changes uids, slots, lookups or stored bytes. Each row below changes the output for some permutation, not for every permutation.

### 1.1 Readers of the node order

| Reader | Evidence | Effect of a reorder |
|---|---|---|
| uid assignment | `assign_file` numbers declarations "in slice order" (`ecp-core/src/contracts/identity.rs:411-415`). A later equal base uid gets `UidSlot::Later { ordinal }` (`:318-333`, `:507`). Anchor positions count up per anchor (`:480`, `:523-524`). | Two overloads swap uids. |
| builder pass 1 | `file_identities(&path_str, &local_graph.nodes, …)` (`resolution/builder.rs:1275`), then one node per raw node in order (`:1283`). The first uid wins and later ones become tombstones (`uid_seen`, `:1228`, `:1343`, `:1465-1499`). | Survivor and tombstone swap. |
| global slots | `RawIndexMap::new(0, …nodes.len())` (`builder.rs:317`; `ecp-core/src/contracts/raw_records.rs:637`) maps raw index `i` to a slot file after file. | Every node index in `graph.bin` moves. |
| raw-index records | `RawTxScope` packs a node index (`ecp-core/src/analyzer/types.rs:1281-1322`). `tx_scope_edges` reads it (`post_process/tx_scope_edges.rs:76`, `:82`). | The scope binds another function. |
| first-of-name lookup | `lookup_in_file` returns `ids.first()` (`resolution/index.rs:1178-1183`). | Another same-name node gets the edge. |
| innermost enclosing, first-minimum tie | `enclosing_fn_idx_by_span` uses `min_by_key`, which keeps the first of equal areas (`framework_helpers.rs:201-213`). | A tie binds another node. |
| call attachment, first-minimum tie | `attach_to_enclosing_span` (`calls.rs:156-176`) takes the first callable of minimum area (`min_by_key`, `:170`) and appends the call to that node (`:172-175`). The returned `(node, call)` pair fills `CallSiteIndex` (`calls.rs:152`). | `LocalGraph` changes inside the parser: the call moves to another node, and the indirect-dispatch detectors read another call index. |
| lexical twin, last-maximum tie | `twin_enclosing` uses `max_by_key` (`ecp-core/src/analyzer/lexical.rs:141-150`, `:148`). `max_by_key` keeps the last of equal keys. | A tie selects the later candidate, the opposite end from the two rows above. |
| PHP namespace | `file_namespace` takes the first `Namespace` node (`php/parser.rs:926-929`) and feeds the Laravel route names (`:930-933`). | Route names take another namespace before the builder runs. |
| overlay | Fragment symbol `i` is raw node `i` (`ecp-cli/src/session/overlay_writer.rs:224-228`). | Overlay slots point at other nodes. |
| call metadata | `RawCallMeta.call_index` is a position in `RawNode.calls` (`types.rs:1381-1388`). The builder pairs by `(caller_span, call_index)` (`builder.rs:4158`). | Indirect-call metadata attaches to another call. |

### 1.2 Readers of the import order and the side-table order

| Reader | Evidence | Effect of a reorder |
|---|---|---|
| name resolution | `raw_imports.iter().find(…)` takes the first import that binds the name (`resolution/resolver.rs:2020-2026`; also `:1064-1072`, `:2297`). | A name resolves through another import. |
| import facts | One `ImportFact` per import, in order (`builder.rs:1079-1100`). | Edge order changes. |
| string pool | `StringPool::add` appends a new string at the end (`ecp-core/src/pool.rs:30-40`). | String offsets in `graph.bin` change. |
| function meta | The overlay keys metas by span with `collect()` into a `HashMap`, so the last duplicate wins (`overlay_writer.rs:219`). | Another meta wins a span tie. |
| blind spots, framework refs, routes | The builder reads them in order (`builder.rs:539`, `:2834`, `:3920`, `:1996`). | Record and edge order in `graph.bin` change. |

### 1.3 The parse cache makes byte identity a hard rule

An extraction slice does not bump `BUILDER_FINGERPRINT` (`ecp-core/src/registry/commit_meta.rs:166`). The parse-cache directory is keyed by it (`ecp-cli/src/parse_cache.rs:31-36`). So the new binary reads `LocalGraph` entries that the old binary wrote. If the new parser emitted another order, one source would give two graphs, chosen by cache state.

### 1.4 Consequences

- The driver keeps each language's current order of node pushes, import pushes and side-table pushes.
- The T2-T7 proof stays order-sensitive (§6.3). `crates/ecp-cli/tests/local_graph_dump.rs` sorts graphs by path only (`:37`). Keep it so. Never sort records inside one `LocalGraph`.
- The driver never builds a record list from a `HashMap` iteration.

## 2. What the driver owns and what the spec owns

### 2.1 Ownership

| Concern | Driver | Spec |
|---|---|---|
| tree parse | the tree phase (§2.2): the optional source patch, `Pass::Tree`, the optional tree fix-up, the error text | the thread-local `Parser` (`fn parse_tree`) |
| query | `Compiled::new` compiles once; one `QueryCursor` loop per file | `fn query()` (the `.scm` text) |
| capture dispatch | the capture-index table: kind captures, unconditional blind spots, the anonymous capture, taps | `type Role` and `fn role(name)` for every other capture, conditional blind spots included |
| per-match emit | creates and resets `DeclSlot` and `Match` | `fn capture` and `fn end_match`: the order of pushes inside one match |
| node construction | `node::decl`, `node::anonymous`, `node::span*` | the field values |
| imports | `imports::raw`, `ImportOrder` sort and dedupe | the source, name, alias and `binding_kind` values |
| call attachment | owns `nodes`, `call_sites`, `path_literals`, `sql_refs`; runs the step | the call extractor (`receiver_types.rs`) as a hook |
| field and variant reads | `Op::FieldReads`, `Op::VariantRefs` | `const FIELD_READS`, `const VARIANT_REFS` |
| function meta | `Op::FunctionMeta`, with the driver's file category | `fn function_meta` (the existing `function_meta::<lang>::extract`) |
| owner stamping | `Op::StampOwners`, `Op::StampOwnerFns`, `Op::ClearScopedOwners` | `const SCOPE_LANGUAGE`, extra stamp passes as hooks |
| tx scopes | `Op::TxScopes`, a bind step | `const TX: TxRule`, with its accepted-kind list |
| event topics | the tap, `Op::EventTopics`, `Op::EventTopicsRequery` | `const EVENT_TOPICS` |
| schema fields | the tap, `Op::SchemaFields`, `Op::SchemaFieldsRequery` | `const SCHEMA_FIELDS` |
| test classification | category and `is_test`, once per file | `const TEST_RULE` |
| pass order | runs `STEPS` in order; `const` phase check | the `STEPS` list |
| `LocalGraph` | assembles every field with one empty-to-`None` rule | `fn finish` moves spec-only side tables in |

### 2.2 Rust shape

```rust
// driver/mod.rs
pub trait ParseSpec: LangSpec + Sized + 'static {
    type Role: Copy;                 // the spec's own captures
    type Match<'t>: Default;         // per-match buffers, as today's loop locals
    type File<'t>: Default;          // per-file side tables (route_arguments, fn_spans, ...)
    type Hook: Copy;

    const PARSE_ERROR: &'static str = "Failed to parse file";
    const KIND_CAPTURE: KindCapture;                 // NameOverwrite | NameKeepFirst | RootOverwrite
    const ANONYMOUS: Option<Anonymous> = None;       // capture name, call kinds, AnonKey
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[]; // unconditional only
    const TEST_RULE: TestRule = TestRule::Category;
    const SCOPE_LANGUAGE: ScopeLanguage = ScopeLanguage::FromPath;
    const FIELD_READS: Option<FieldReads> = None;    // member kinds + NameRefSpec
    const VARIANT_REFS: Option<VariantRefSpec> = None;
    const EVENT_TOPICS: &'static [EventTopicConfig] = &[];
    const SCHEMA_FIELDS: &'static [SchemaFieldConfig] = &[];
    const TX: TxRule = TxRule::None;
    const PREPARE_SOURCE: Option<fn(&[u8]) -> Option<Vec<u8>>> = None;  // C++
    const POST_TREE: Option<fn(Tree, &[u8]) -> Tree> = None;            // Kotlin
    const STEPS: &'static [Step<Self::Hook>];

    fn query() -> anyhow::Result<Query>;
    fn role(capture: &str) -> Option<Self::Role>;
    fn parse_tree(source: &[u8]) -> Option<Tree>;

    fn capture<'t>(r: Self::Role, node: Node<'t>, d: &mut DeclSlot<'t>,
                   m: &mut Self::Match<'t>, f: &mut Self::File<'t>, cx: &mut FileCx<'t>);
    fn end_match<'t>(d: DeclSlot<'t>, m: &mut Self::Match<'t>,
                     f: &mut Self::File<'t>, cx: &mut FileCx<'t>);
    fn hook<'t>(h: Self::Hook, f: &mut Self::File<'t>, cx: &mut FileCx<'t>);
    fn function_meta(root: Node<'_>, source: &[u8], nodes: &[RawNode],
                     category: FileCategory) -> Vec<RawFunctionMeta>;
    fn finish<'t>(_f: Self::File<'t>, _g: &mut LocalGraph) {}
}

pub enum KindCapture { NameOverwrite, NameKeepFirst, RootOverwrite }
pub struct Step<H> { pub pass: Pass, pub effect: Effect, pub op: Op<H> }
pub enum Effect { Nodes, Neutral, Bind }
pub enum Op<H> {
    Query, Hook(H), SortDedupImports(ImportOrder), DedupNodes,
    FieldReads, VariantRefs, FunctionMeta,
    StampOwners, StampOwnerFns, ClearScopedOwners,
    TxScopes,
    EventTopics, EventTopicsRequery,     // EventTopics: added by the tap commit (§6.2)
    SchemaFields, SchemaFieldsRequery,   // SchemaFields: added by the tap commit (§6.2)
}
pub enum TxRule {
    None,
    Nodes { collect: fn(&[RawNode], &[NodeKind]) -> Option<Box<[RawTxScope]>>, kinds: &'static [NodeKind] },
    Tree(fn(Node<'_>, &[u8], &[RawNode]) -> Option<Box<[RawTxScope]>>),
    Sites,                               // Python: resolve_tx_scopes(nodes, &cx.tx_sites)
}
pub const fn steps_ok<H>(steps: &[Step<H>]) -> bool;  // see §3.1

pub struct Compiled<S: ParseSpec> { query: Query, table: Box<[Entry<S::Role>]>, taps: TapTable }
impl<S: ParseSpec> Compiled<S> { pub fn new() -> anyhow::Result<Self>; }
pub fn parse<S: ParseSpec>(c: &Compiled<S>, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph>;
```

A provider keeps its public API. `GoProvider { compiled: Compiled<GoSpec> }`, and `parse_file` calls `driver::parse(&self.compiled, path, source)`. The `impl ParseSpec for GoSpec` block lives in `go/parser.rs`, so `go/spec.rs` stays unchanged.

**Tree phase.** `parse` runs these three parts before `STEPS`, in this order:

1. When `PREPARE_SOURCE` is `Some`, call it under `Pass::Walk`. C++ uses `blank_decl_prefix_macros` (`cpp/recovery.rs:149`).
2. Under `Pass::Tree`, call `parse_tree` on the patched bytes if a patch exists, else on `source`. Drop the patch at once (`cpp/parser.rs:317-329`). The query cursor, every capture text, every hash and every later step read the original `source` (`cpp/parser.rs:314-315`, `:365`).
3. When `POST_TREE` is `Some`, call it under `Pass::Walk`. Kotlin uses `recover_same_line_closers` (`kotlin/parser.rs:136`, called at `:317-318`).

A `None` constant removes its branch at compile time, so the other 12 languages get no extra span.

**Tree ownership.** The `Tree` stays a local of `parse`. The driver borrows it into the context: `FileCx<'t>` holds `tree: &'t Tree` and `source: &'t [u8]`. `Match<'t>`, `File<'t>` and `DeclSlot<'t>` hold `Node<'t>` values from that borrow. `POST_TREE` takes the `Tree` by value before the borrow starts, so it needs no other lifetime.

`FileCx<'t>` also holds `path`, `category`, `is_test`, `nodes`, `imports`, `blind_spots`, `routes`, `framework_refs`, `fanout_refs`, `path_literals`, `sql_refs`, `call_sites`, `call_metas`, `raw_function_metas`, `tx_sites`, `tx_scopes`, `event_topics`, `schema_fields`, `route_receivers`, `export_aliases`, the anonymous dedupe sets (§2.4) and the tap rows (§2.5). These are today's `parse_file` locals with the same types.

**Slot semantics.** `DeclSlot<'t> { name: Option<Node<'t>>, root: Option<Node<'t>>, kind: Option<NodeKind> }`. The driver writes it only for a kind capture:

| `KIND_CAPTURE` | A kind capture writes | Languages | Evidence |
|---|---|---|---|
| `NameOverwrite` | `name` and `kind`, on every capture | Go, TS, JS, Python, Java, Kotlin, C#, PHP, Swift, C, C++, Dart | `go/parser.rs:563-564`; same two statements in the other 11 |
| `NameKeepFirst` | `name` on every capture; `kind` only while it is empty | Rust | `rust/parser.rs:240-243` |
| `RootOverwrite` | `root` and `kind`, on every capture; never `name` | Ruby | `ruby/parser.rs:462-473` |

A role may also write `d.name` or `d.root`. Ruby's `@name` capture is a role that writes `d.name` (`ruby/parser.rs:454-455`). Ruby's `end_match` emits only when `name`, `kind` and `root` are all set (`:568`). It reads the name text from `name` (`:569-570`), the span from `root` (`:572-573`, `:595-600`) and hashes the `root` bytes (`:604-606`). Only Ruby has root-kind captures. Every other `CAPTURE_KIND` table names a name capture (the 13 kind arms above). Ruby's table holds `class`, `module`, `method` and `const` (`ruby/spec.rs:24-29`), and the query binds them to declaration roots (`ruby/queries.scm:8`, `:16`, `:26`, `:35`, `:102`).

Every slot write is last-write-wins: a repeated capture of one index replaces the earlier node. A spec field in `Match` keeps today's form: an assignment stays last-write-wins, and a push keeps every capture in capture order. Swift's enum-case names are a push (`swift/parser.rs:388-425`).

### 2.3 Capture-index table

`Compiled::new` maps each capture index to one `Entry { slot: Slot<S::Role>, tap: TapRange }`:

| Slot | Source of truth | Today |
|---|---|---|
| `Kind(NodeKind)` | `S::CAPTURE_KIND` (the existing `LangSpec` table) and `S::KIND_CAPTURE` | the `capture_kind_by_idx` arm (`go/parser.rs:554-564`, `rust/parser.rs:232-243`, `ruby/parser.rs:462-473`) |
| `Blind(i)` | `S::BLIND`: unconditional captures only | `go/parser.rs:672-688`, `rust/parser.rs:321-336` |
| `Anon` | `S::ANONYMOUS.capture` | `go/parser.rs:646-671`, `rust/parser.rs:337-364` |
| `Lang(role)` | `S::role(name)` | the rest of the if-chain, conditional blind spots included |
| `Skip` | none | no arm matches |

The arm order differs per language today. The kind arm is first in 11 languages, but JS tests three variable captures first (`javascript/parser.rs:274-285`), Kotlin tests six Ktor captures first (`kotlin/parser.rs:379-395`), and Ruby tests `name` and `heritage` first (`ruby/parser.rs:454-461`). The order decides an arm only when one capture name matches two arms. So `Compiled::new` returns an error when one name resolves to two of `Kind`, `Blind`, `Anon` and `Lang`. Then no priority rule is needed, and a hidden overlap fails the provider's constructor test instead of changing output. The `tap` range is independent of the slot, so one capture can feed both a slot and a tap.

Each capture index holds one name, so the table gives the same arm as the first matching `cap_idx == idx_*` test. Go today calls `capture_index_for_name` about 25 times per file (`go/parser.rs:445-494`). The table resolves them once per provider.

### 2.4 Shared helpers and the copies they replace

| Helper | Go copies | Rust copies | Other 12 | Same computation | Differences kept as spec parameter |
|---|---|---|---|---|---|
| `node::decl(name, kind, span, hashed) -> RawNode`: empty `decorators`, `heritage`, `calls`, `field_reads`; `is_exported: false`; `type_annotation`, `owner_class`: `None`; `content_hash = xxh3_64_bytes(hashed)`. The caller sets other fields with struct-update syntax. | `:705`, `:750`, `:829`, `:876`, `:928` | `:504` | 40 `RawNode {` literals in the 12 files (TS 2, JS 3, Python 3, Java 2, Kotlin 2, C# 3, PHP 2, Ruby 4, Swift 6, C 4, C++ 5, Dart 4), some outside `parse_file` | Every literal sets the same 11 fields (`ecp-core/src/analyzer/types.rs:484-518`). The empty values are the same. | The span source and the hashed range differ per copy: Go `@var` spans name start to `var_spec` end and hashes the root (`:748-768`). The caller passes both, and every non-default field explicitly. |
| `node::span(node)`, `node::span_from(start, end)` | `:712-717`, `:757-762`, `:835-840`, `:882-887`, `:935-940` (`as u32`) | `node_span` (`:400`) | all 12 | `node_span` saturates with `try_from` (`framework_helpers.rs:47-56`). `as u32` truncates. Both are equal below 2^32. The parse budget is not a hard length check: it cancels the parse at a progress callback once the offset passes 8 MiB (`parse_budget.rs:16-19`, `:32`). So a returned tree covers about 8 MiB of source, far below 2^32 rows or columns. | none; each copy keeps its endpoint choice |
| `node::anonymous(node, source)` and `node::subtree_has_kind(node, kinds)` | `:646-671`, gate `subtree_contains_kind(…, "call_expression")` | `:337-364`, gate `body_has_call` (`:47-59`) | 11 more (C has no anonymous node). `body_has_call` exists in 12 files. | Name `<anonymous:{row+1}:{col}>`, kind `Function`, span `node_span`, hash of the node bytes (Java `:321` formats `start.row + 1` as `usize`: same digits). Every gate returns "a descendant or the node itself has one of these kinds", so the traversal order cannot change the result. | `Anonymous.call_kinds`: `call_expression` (Go, Rust, TS, JS, Kotlin, Swift, C++, Dart), `call` (Python, Ruby), `method_invocation` + `object_creation_expression` (Java), `invocation_expression` (C#), four PHP call kinds (`php/parser.rs:97-115`). `Anonymous.key`: see the dedupe-set paragraph below. |
| `imports::raw(source, imported_name, alias, binding_kind) -> RawImport` | `:1020-1025` | `:536-541`, `push_import` `:854-859` | 25 `RawImport {` literals | A struct literal with the same four fields. `binding_kind` is an explicit argument with no default. | `binding_kind`: `None` in most copies. C passes `Some(Alias)` (`c/parser.rs:349`, `:436`), a computed kind (`:380`) and `Some(Macro)` (`:418`). TS and JS pass `js_ts_reexport_kind(i_src)` (`typescript/parser.rs:715`, `javascript/parser.rs:605`). The text rules (Go quote trim and last segment, `:995-1018`) stay in the spec. |
| `ImportOrder` sort and dedupe | `SourceName`: `:1046-1051` | none | C++ `NameSourceAlias`: `cpp/parser.rs:722-730` | The same stable `sort_by` and `dedup_by` calls, moved. | `SourceName`: sort by `(source, imported_name)`, dedupe on both; `alias` is not a key, so the stable sort keeps the first alias. `NameSourceAlias`: sort by `(imported_name, source, alias)`, dedupe on all three. |
| `Op::DedupNodes` | none | none | Dart: `nodes.dedup_by` on name, span and kind (`dart/parser.rs:492`) | `Vec::dedup_by` removes consecutive duplicates only. The op calls the same function. | none. A global dedupe is not equivalent: it also removes a duplicate that another node separates. |
| framework gate | `has_import_from` `:1110-1111`; source rule `enclosing_function_name(..).unwrap_or(MODULE_LEVEL_SOURCE)` `:1142-1143` | `has_import_from` `:632-633`; axum innermost byte range `:640-643`; actix module source `:661` | `has_import_from` in TS, JS, Python, Java, Kotlin, PHP; text detector `detect_ast_framework_patterns` in C#, Ruby, Swift, C++, Dart; C none | `has_import_from` is already one function (`framework_helpers.rs:770`). The driver adds one helper, `node::enclosing_or_module(nodes, span)`, for the identical expression in Go `:1142-1143`, TS `:953-954`, JS `:771-772` and `:784-785`, PHP `:1043-1044`. | The pending-ref construction stays in the spec: confidence, reason and source rule differ per framework. Rust axum drops a ref with no enclosing byte range; Go falls back to `<module>`. |
| owner stamping | none: Go sets owners at emit time (`:912-916`) | `stamp_owner_class_by_span` `:705`; `clear_scoped_owners(Rust, …)` `:708-711` | `stamp_owner_class_by_span` in all 12; `stamp_owner_fn_by_span` in TS, JS, Python; `clear_scoped_owners` in TS, JS, Python | The op calls the same functions. They write `owner_class` only (`framework_helpers.rs:344`, `:448`; `ecp-core/src/analyzer/lexical.rs:66`). | `ScopeLanguage::Fixed(Language::Rust)` for Rust (`:709`). `ScopeLanguage::FromPath` for TS (`typescript/parser.rs:1005`), JS (`javascript/parser.rs:824`), Python (`python/parser.rs:1732`). |
| function meta | `function_meta::go::extract` `:1153-1154` | `function_meta::rust_lang::extract` `:672-677` | one `extract` per language, all 14 | All 14 already share `extract_with` (`function_meta/mod.rs:119-145`). The op makes the same call with the driver's category. | none; the spec names its `extract`. The step keeps its current place relative to promotions and owner stamping. |
| field and variant reads | `extract_field_reads_with_literal_key_facts` `:1069-1097` (a hook: it needs `literal_key_facts`) | `extract_field_reads` `:568-585`, `extract_variant_refs` `:586-599` | field reads in all 12; variant refs in 9 of them (not JS, Ruby) | Both functions are shared already (`calls.rs:219-227`). `extract_field_reads` is the `None` case of the Go form (`calls.rs:226`). | `FieldReads { member_kinds, names: NameRefSpec }` and `VariantRefSpec` as `const` per spec. |
| tx scopes | tree walk `collect_go_sql_tx_scopes` `:330-368`, call `:1161` | `collect_rust_transactional_scopes` `:713-714` | decorator form: Java, Kotlin, C#, PHP, TS; tree walk: Ruby, Dart; pending sites: Python; none: JS, Swift, C, C++ | The op calls the language's existing function with the language's existing arguments. | `TxRule::Nodes { collect, kinds }`: the function and the accepted-kind list each caller passes today (table below). `TxRule::Tree(f)`: Go `:330`, Ruby `ruby/parser.rs:259`, Dart `dart/parser.rs:623`. `TxRule::Sites`: Python `resolve_tx_scopes` (`python/parser.rs:1854-1868`). Go dedupes per function (`:349`); Python does not. |
| event topics, schema fields | `extract_event_topics` with 4 configs `:1163-1177` | 4 configs `:679-693` | events: TS, JS, Python, Java, Kotlin; schemas: TS 1, Python 2 | See §2.5. | `const EVENT_TOPICS`, `const SCHEMA_FIELDS` |
| blind-spot captures | `:672-688` | `:321-336` | all 12 call `push_blind_spot` | For a `Blind` slot, the driver calls the same `push_blind_spot` (`framework_helpers.rs:103-117`) at the same point of the capture loop. | `const BLIND` holds unconditional captures only. A conditional capture is a role (table below). |
| test classification | `:437-439` | `:197-199` | 11 identical copies; Python differs | `determine_category(path.to_str().unwrap_or(""))`, then `== FileCategory::Test`. | `TestRule::PythonPath`: `is_test_path(&path.to_string_lossy())` (`python/parser.rs:1022`) and a category of `Test` or `Source` only (`:1665-1669`). |

The helpers do not replace `receiver_types.rs` (14 copies) in T2. Each copy computes a different language's call binding, so they are not one computation.

**Tx accepted-kind lists.** `TxRule::Nodes` passes `kinds` to `collect` unchanged. Keep each list as written today:

| Lang | `collect` | `kinds` | Evidence |
|---|---|---|---|
| rust | `collect_rust_transactional_scopes` | `[Function, Method]` | `rust/parser.rs:713-714` |
| typescript | `collect_typeorm_transactional_scopes` | `[Method, Function]` | `typescript/parser.rs:1011-1012` |
| java | `collect_jvm_transactional_scopes` | `[Method, Constructor]` | `java/parser.rs:577-578` |
| kotlin | `collect_jvm_transactional_scopes` | `[Method, Function, Constructor]` | `kotlin/parser.rs:695-698` |
| c_sharp | `collect_dotnet_transactional_scopes` | `[Method, Constructor, Function]` | `c_sharp/parser.rs:808-811` |
| php | `collect_symfony_transactional_scopes` | `[Method, Function]` | `php/parser.rs:1137-1138` |

All five functions take `(nodes, scopeable_kinds)` and return `Option<Box<[RawTxScope]>>` (`framework_helpers.rs:859`, `:887`, `:912`, `:941`, `:967`).

**Conditional blind spots.** These captures push a blind spot only when a predicate holds. Each one is a `Lang` role. The role's `capture` code keeps the predicate and pushes at the same point of the capture loop, so the order of `blind_spots` stays the same:

| Lang | Capture | Push only when | Evidence |
|---|---|---|---|
| typescript | `blind.dynamic_import` | `!js_ts_first_arg_is_literal_string(&node)` | `typescript/parser.rs:565-574`; predicate `framework_helpers.rs:72` |
| typescript | `blind.dynamic_require` | `!js_ts_first_arg_is_literal_string(&node)` | `typescript/parser.rs:575-584` |
| javascript | `blind.dynamic_import` | `!js_ts_first_arg_is_literal_string(&node)` | `javascript/parser.rs:397-406` |
| javascript | `blind.dynamic_require` | `!js_ts_first_arg_is_literal_string(&node)` | `javascript/parser.rs:407-416` |
| javascript | `blind.object_freeze_enum` | `is_object_freeze_enum(&node)` | `javascript/parser.rs:417-426`; predicate `:52` |
| php | `blind.call_user_func` | `literal_callable(node, source).is_none()` | `php/parser.rs:725-734`; predicate `php/receiver_types.rs:266` |
| ruby | `blind.send` | `literal_send_target(node, source).is_none()` | `ruby/parser.rs:522-531`; predicate `ruby/receiver_types.rs:155` |

Ruby tests the predicate in the `else if` condition. A literal target falls through to the next arm, `idx_function_anonymous` (`:532`), which has another index, so the fall-through matches nothing. A role that does nothing on a literal target gives the same result. Every other in-loop blind capture of the 14 parsers is unconditional and goes into `BLIND`: Go 2, Rust 2, TS 2, JS 2, Python 6 (dispatched in the last `else`, `python/parser.rs:1235-1254`), Java 2, Kotlin 2, C# 2, PHP 2 (`blind.eval`, `blind.variable_call`), Ruby 2, Swift 2, C 1, C++ 1, Dart 2. Blind spots pushed outside the capture loop stay in their hooks at their current step: Go iota (`go/parser.rs:442`), TS freeze enums (`typescript/parser.rs:838`), Python enum imitation (`python/parser.rs:880`), PHP enum imitation (`php/parser.rs:1098`) and dynamic calls (`:979-985`), Ruby module-as-enum (`ruby/parser.rs:863`) and Rails route depth (`:994-1001`).

**Anonymous dedupe sets.** `FileCx` owns two sets: `anon_spans: FxHashSet<Span>` and `anon_ids: FxHashSet<usize>`. An empty set allocates nothing. `Anonymous.key` selects how the anonymous emit tests a candidate:

| `AnonKey` | Test before the push | Languages | Evidence |
|---|---|---|---|
| `Span` | `anon_spans.insert(span)` | Go, Rust, TS, JS, Kotlin, C#, PHP, Ruby, Swift, C++, Dart | `go/parser.rs:654`, `rust/parser.rs:346`, `ruby/parser.rs:547` |
| `NodeId` | `anon_ids.insert(node.id())` | Java | `java/parser.rs:197`, `:313` |
| `AnyNodeSpan` | no node in `cx.nodes` has this span | Python | `python/parser.rs:1217` |

JavaScript shares its span set between the anonymous emit and its declarations. Today one set `emitted_spans` (`javascript/parser.rs:198`) takes the anonymous inserts (`:359`) and the declaration inserts (`:482`), and the variable emit reads it (`:544`). The driver keeps exactly one shared set: JS `end_match` inserts its declaration spans into `cx.anon_spans` and tests its variables against `cx.anon_spans`. TS has a set of the same name, but only the anonymous emit uses it (`typescript/parser.rs:431`, `:592`). The step from std `HashSet` to `FxHashSet` changes no output, because no copy iterates the set.

### 2.5 The record tap

Today `extract_event_topics` (`event_topic/extract.rs:31-148`) and `extract_schema_fields` (`schema_field/extract.rs:28-136`) each run the whole query a second time, with a default cursor (`event_topic/extract.rs:75-76`, `schema_field/extract.rs:75-76`). Go and Rust pass their own main query for events (`go/parser.rs:1164-1176`, `rust/parser.rs:680-693`). TS passes its main query for one schema config and four event configs (`typescript/parser.rs:1018-1041`). Python passes its main query for two schema configs and five event configs (`python/parser.rs:1742-1769`).

The tap gives the same records from the main pass. The two families differ in four rules, and the tap keeps each one per family:

| Rule | Event topics | Schema fields |
|---|---|---|
| config type | `EventTopicConfig` (`event_topic/config.rs:16-45`) | `SchemaFieldConfig` (`schema_field/config.rs:13-31`) |
| slot roles | `[topic, producer, direction]` | `[owner, name, type]` |
| an empty capture name | `producer` or `direction` gets no role (`event_topic/extract.rs:59-70`) | `type` gets no role (`schema_field/extract.rs:65-70`) |
| a config is active when | `has_import_from(imports, gate)` (`event_topic/extract.rs:39-42`). An empty gate is never active: `has_import_from` tests `modules.iter().any(…)` (`framework_helpers.rs:788-791`). | `gate.is_empty() \|\| has_import_from(imports, gate)` (`schema_field/extract.rs:38-41`). An empty gate is always active: TS interfaces need no import (`typescript/schema_extractors.rs:12-19`). |
| required slots for a candidate | `topic` (`event_topic/extract.rs:107`) | `owner` and `name` (`schema_field/extract.rs:108`) |
| record | `RawEventTopic` as `event_topic/extract.rs:111-142` | `RawSchemaField` as `schema_field/extract.rs:112-130` |

So TS interface schemas never take the event-gate rule. Each family has its own active test, and the step for one family never reads the other family's rows.

**Table.** `Compiled::new` gives each config a tap index: events first, then schemas, each in declaration order. For every config and every role with a non-empty capture name that the query holds, it adds a `TapTarget { cfg: u8, role: u8 }` to the capture's `TapRange`. It adds targets for every config, not only active ones, because the gates read the imports after the loop. A spec with no configs has no targets.

**Per match.** The driver keeps one reusable slot table, `slots: Box<[[Option<(u32, u32)>; 3]]>`, with one entry per config, and one `dirty` flag.

1. For each capture in `m.captures()` order, the driver first writes every tap target: `slots[cfg][role] = Some((start_byte, end_byte))`. A repeated capture overwrites, so the last write wins, as today (`event_topic/extract.rs:98-100`, `schema_field/extract.rs:99-101`). Then the driver dispatches the slot (§2.3).
2. After the capture loop and before `S::end_match`, a dirty match closes its group. The driver walks the configs in tap order. It keeps one `TapRow { seq, cfg, span, slots: [Option<(u32, u32)>; 3] }` for each config whose required slots are set. `seq` is the ordinal of the tapped match. `span` is `m.captures()[0]` as `(start.row as u32, start.column as u32, end.row as u32, end.column as u32)` (`event_topic/extract.rs:125-132`, `schema_field/extract.rs:114-121`). Then it resets the slots and the flag.
3. `S::end_match` runs after step 2. So a spec's early exit cannot skip a row. Rust's `continue` at `rust/parser.rs:387` leaves the whole match; it becomes an early return from `end_match`. Go's `_` name exits (`go/parser.rs:744`, `:791`, `:870`) skip one name inside an inner `for` (`:739`, `:787`, `:865`), and they stay as they are.

**At `Op::EventTopics` and `Op::SchemaFields`.** The step computes the family's active flags once, with the family's rule above.

- If no config of the family is active, the step drops the family's rows, decodes no text and leaves the field empty, so the assembly writes `None`. No row of the family outlives the step.
- Otherwise the step walks the rows in order. Rows with the same `seq` form one match group. In each group it emits the record of the first row whose config is active, then skips the rest of the group. So each match emits at most one record per family, as today (`event_topic/extract.rs:104-144`, `schema_field/extract.rs:105-132`).
- Text decode: `std::str::from_utf8(&source[a..b]).unwrap_or("")`. This is the body of `Node::utf8_text`, so invalid UTF-8 gives `Some("")`, as today (`event_topic/extract.rs:97`, `schema_field/extract.rs:98`). An event with an empty topic text still emits.
- A role with no slot gives `None`. SQS for Rust has an empty producer capture (`event_topic/sqs.rs:83`). Its producer slot stays `None`, so `enclosing_fn` is `""` (`event_topic/extract.rs:123`), and the record still emits.

**Why the output is the same.**

- One query on one tree with a default cursor gives the same matches in the same order. Go and Rust create a default `QueryCursor` with no match limit and no byte range (`go/parser.rs:431-432`, `rust/parser.rs:187-188`). The re-query does the same.
- Each config's slots depend only on that config's captures (`event_topic/extract.rs:94-101`).
- Filtering the configs by the gate keeps their order. So "the first active config with its required slots" is the same in both forms.
- The step sits where the call sits today, so it reads the same imports.

**Buffer and bound.** The tap adds one per-file buffer, `FileCx.tap_rows: Vec<TapRow>`, and one per-provider slot table. A row is at most 64 bytes: it holds ranges, not strings. The buffer holds at most one row per config per tapped match: at most 4 rows per match for Go and Rust. It grows by doubling, so a file pays O(log n) allocations for n rows. A file with no tapped capture allocates nothing. The buffer is freed when `parse` returns. A file with tapped captures but no active config now pays for its rows during the query. Today's extractor returns at once for such a file (`event_topic/extract.rs:44-46`). The perf rule of §5.3 measures this cost.

**Fallback ops.** `Op::EventTopicsRequery` and `Op::SchemaFieldsRequery` call `extract_event_topics` and `extract_schema_fields` as today, with `S::EVENT_TOPICS` or `S::SCHEMA_FIELDS`. The pure moves use them. If the tap commit fails §5.3, they stay.

### 2.6 Language matrix

Capture and node parameters:

| Lang | `KIND_CAPTURE` | Anonymous gate kinds, key | Conditional blind roles | Test rule | Tree phase, before the query | Other spec parameter |
|---|---|---|---|---|---|---|
| go | name overwrite | `call_expression`, span | none | category | none; iota walk hook | per-function tx dedupe (`go/parser.rs:349`) |
| rust | name keep-first | `call_expression`, span | none | category | none | declaration merge by priority in `end_match` (`rust/parser.rs:470-519`) |
| typescript | name overwrite | `call_expression`, span | `dynamic_import`, `dynamic_require` | category | none; new `Parser` per file | freeze-enum hook |
| javascript | name overwrite | `call_expression`, span, shared with declarations | `dynamic_import`, `dynamic_require`, `object_freeze_enum` | category | none | bare `<anonymous>` name (`javascript/parser.rs:336`) |
| python | name overwrite | `call`, any node span | none | python path | import pre-scan hook (§2.7) | blind dispatch in the last `else` |
| java | name overwrite | 2 kinds, node id | none | category | none | `node_id_to_idx` in `File` (`java/parser.rs:194`) |
| kotlin | name overwrite | `call_expression`, span | none | category | `POST_TREE` | `node_id_to_idx` in `File` (`kotlin/parser.rs:327`) |
| c_sharp | name overwrite | `invocation_expression`, span | none | category | none | `node_id_to_idx` in `File` (`c_sharp/parser.rs:299`) |
| php | name overwrite | 4 kinds, span | `call_user_func` | category | none | first `Namespace` node for routes (`php/parser.rs:926-929`) |
| ruby | root overwrite | `call`, span | `send` | category | visibility walk hook | `initialize` promotion to `Constructor` (`ruby/parser.rs:583-587`) |
| swift | name overwrite | `call_expression`, span | none | category | none | enum-case names pushed in capture order, one node per name, shared span and root hash (`swift/parser.rs:395-425`) |
| c | name overwrite | no anonymous node | none | category | none | none |
| cpp | name overwrite | `call_expression`, span | none | category | `PREPARE_SOURCE`; override pre-scan hook (§2.7) | none |
| dart | name overwrite | `call_expression`, span | none | category | none | `DedupNodes`, consecutive only (`dart/parser.rs:492`) |

Record and pass parameters:

| Lang | Field reads, variant refs | Tx rule | Event, schema configs | Owner ops | Imports |
|---|---|---|---|---|---|
| go | hook (literal keys), none | tree `collect_go_sql_tx_scopes` | 4, 0 | none | sort `SourceName`; `binding_kind` `None` |
| rust | const, const | nodes rust `[Function, Method]` | 4, 0 | stamp, clear (fixed language) | nested `use` hook; `None` |
| typescript | const, const | nodes typeorm `[Method, Function]` | 4, 1 (empty gate) | stamp, stamp fns, clear | binding marks hook; `js_ts_reexport_kind` |
| javascript | const, none | none | 4, 0 | stamp, stamp fns, clear | binding marks hook; `js_ts_reexport_kind` |
| python | const, const | sites | 5, 2 | stamp, stamp fns, clear | root-import hook at the end; `None` |
| java | const, const | nodes jvm `[Method, Constructor]` | 4, 0 | stamp | `None` |
| kotlin | const, const | nodes jvm `[Method, Function, Constructor]` | 1, 0 | stamp | `None` |
| c_sharp | const, const | nodes dotnet `[Method, Constructor, Function]` | 0, 0 | stamp | `None` |
| php | const, const | nodes symfony `[Method, Function]` | 0, 0 | stamp, const owners hook | `None` |
| ruby | const, none | tree `collect_ruby_transaction_scopes` | 0, 0 | stamp | delegator bindings hook; `None` |
| swift | const, const | none | 0, 0 | member owners hook, stamp | `None` |
| c | const, const | none | 0, 0 | receiver owners hook, stamp | named bindings hook; `Alias`, computed, `Macro` |
| cpp | const, const | none | 0, 0 | lexical owners hook, stamp, friend hook | sort `NameSourceAlias`; `None` |
| dart | const, const | tree `collect_dart_tx_scopes` | 0, 0 | stamp | `None` |

### 2.7 Approved exceptions to the one-query target

The slice plan requires that "One query pass feeds imports, schema fields, event topics and override markers" (`docs/plans/2026-10-26-phase3-area10-slices.md:20`). Two languages keep a second run of the query after their move. Each one is an approved intermediate exception. The one-query target is not complete while either remains.

| Lang | Pre-scan | Why a tap is not enough | Slice that keeps it | Follow-up that removes it |
|---|---|---|---|---|
| python | import pre-scan (`python/parser.rs:961-1010`) | The main loop gates FastAPI, Django and Celery captures on imports at capture time (`:1012-1015`, `:1127-1186`). The imports are complete only after the loop. | T4 keeps it as a pre-query hook under `Pass::Walk`. | Defer the gates: buffer the gated captures as pending refs in the loop, then apply `has_fastapi`, `has_django` and `has_celery` after it. Prove it with the dump, then delete the pre-scan. File the follow-up when T4 closes. |
| cpp | override pre-scan (`cpp/parser.rs:332-360`) | The `override_marker` capture and the method declaration fire in different matches (`:333-337`). The declaration emit reads the marker set at once and puts the sentinel first in `decorators` (`:570-574`). | T7 keeps it as a pre-query hook under `Pass::Walk`. | Tap the marker into a set of `function_definition` ids, then insert the sentinel at decorator index 0 of each marked node after the loop. That is a node-changing step; C++ has no bind step. Prove it with the dump, then delete the pre-scan. File the follow-up when T7 closes. |

## 3. How the driver keeps each pass order

### 3.1 Mechanism

The driver has no fixed phases with hooks between them. Each spec lists its steps in its current order. The driver runs them one after another:

```rust
for step in S::STEPS {
    let _p = parse_prof::span(step.pass);
    match step.op { Op::Query => run_query::<S>(c, &mut f, &mut cx), Op::Hook(h) => S::hook(h, &mut f, &mut cx), /* fixed ops */ }
}
```

Each step carries its own `Pass`, so the `ECP_PROF` attribution stays as T1 marked it. A T1 span that covers two statements becomes two steps with the same `Pass`. The per-pass total is the same sum.

Each spec states `const _: () = assert!(driver::steps_ok(Self::STEPS));`. The check runs at compile time. In debug builds the driver also calls `ParsePhase::advance` (`ecp-core/src/contracts/raw_records.rs:79-84`) at each phase change. Before the first `Bind` step it advances through `FinalizeNodes`, because `advance` allows no skipped phase.

**What the phase check enforces, and what it does not:**

| Check | Enforces | Does not enforce |
|---|---|---|
| `steps_ok` (compile time) | No step declared `Effect::Nodes` follows a step declared `Effect::Bind`. Each fixed op has its fixed effect: `TxScopes` is `Bind`; `DedupNodes`, `StampOwners`, `StampOwnerFns` and `ClearScopedOwners` are `Nodes`; the record ops are `Neutral`. | The real writes of a hook. A hook gets `&mut FileCx`, so a hook declared `Neutral` can still write `nodes`. |
| `ParsePhase::advance` (debug) | The declared phases advance in contract order. | Same as above: it reads the labels, not the writes. |
| node-count assertion (debug) | After the first `Bind` step, `cx.nodes.len()` stays the same through every later step and through `finish`. This catches a late push or removal. | A late retag or reorder that keeps the length. It also does not check side-table writes. |

The dump of §6.3 is the control for every write the checks cannot see. Code review of each hook's declared effect is the second control. `finish` runs after every step and may only move `File` tables into `LocalGraph` fields that no step reads.

### 3.2 Step lists for the 14 languages

Notation: `H(x)` is a hook, `[P]` is the `Pass`, `B` marks a bind step. Every other step is node-changing or neutral. The tree phase of §2.2 runs before each list. `EventTopics` and `SchemaFields` mean `EventTopicsRequery` and `SchemaFieldsRequery` until the tap commit for that language lands.

| Lang | Steps in order | Move |
|---|---|---|
| go | H(iota)[Walk] · Query · H(merge file vars)[Build] · SortDedupImports[Build] · H(calls)[Calls] · H(literal keys, field reads)[Walk] · H(routes, gin/echo)[Build] · FunctionMeta[Walk] · TxScopes B[Walk] · EventTopics[Walk] | none |
| rust | Query · H(nested use)[Imports] · H(calls)[Calls] · FieldReads · VariantRefs · H(indirect)[Walk] · H(axum, actix)[Build] · FunctionMeta · EventTopics[Walk] · StampOwners · ClearScopedOwners · TxScopes B[Build] | none |
| typescript | Query · H(freeze enum)[Walk] · H(calls)[Calls] · FieldReads · VariantRefs[Walk] · H(routes, express, nestjs)[Build] · H(indirect) · FunctionMeta[Walk] · H(exports, import bindings) · StampOwners · StampOwnerFns · ClearScopedOwners · TxScopes B[Build] · SchemaFields · EventTopics[Walk] | none |
| javascript | Query · H(calls)[Calls] · FieldReads[Walk] · H(routes, receivers, express, hapi)[Build] · H(indirect) · FunctionMeta[Walk] · H(exports, import bindings) · StampOwners · StampOwnerFns · ClearScopedOwners[Build] · EventTopics[Walk] | none |
| python | H(import pre-scan)[Walk] · Query · H(calls)[Calls] · FieldReads · VariantRefs · H(indirect)[Walk] · H(depends, gates, fan-out, router prefixes, route dedupe)[Build] · FunctionMeta[Walk] · H(protocol promotion, enum variants) · StampOwners · StampOwnerFns · ClearScopedOwners · TxScopes B[Build] · SchemaFields · EventTopics[Walk] · H(root imports)[Imports] | TxScopes moves from before the promotion (`python/parser.rs:1677`) to after `ClearScopedOwners` (`:1736`) |
| java | Query · H(spring gate)[Build] · H(calls)[Calls] · FieldReads · VariantRefs · FunctionMeta[Walk] · StampOwners · TxScopes B[Build] · EventTopics[Walk] | TxScopes moves after StampOwners (`java/parser.rs:577` before `:580`) |
| kotlin | (tree: `POST_TREE`[Walk]) · Query · H(calls)[Calls] · FieldReads · VariantRefs[Walk] · H(ktor gate)[Build] · FunctionMeta[Walk] · StampOwners · TxScopes B[Build] · EventTopics[Walk] | TxScopes moves after StampOwners (`kotlin/parser.rs:695` before `:700`) |
| c_sharp | Query · H(calls)[Calls] · FieldReads · VariantRefs · H(text frameworks) · FunctionMeta[Walk] · StampOwners · TxScopes B[Build] | none |
| php | Query · H(laravel routes)[Build] · H(calls, fan-out, dynamic blind spots)[Calls] · FieldReads · VariantRefs[Walk] · H(laravel gate, route scope, middleware)[Build] · FunctionMeta · H(enum imitation)[Walk] · StampOwners · H(const owners)[Build] · H(qualified constant refs)[Walk] · TxScopes B[Build] | none |
| ruby | H(visibility map)[Walk] · Query · H(module as enum)[Walk] · H(mixins, delegator nodes)[Build] · H(calls)[Calls] · FieldReads · H(text frameworks)[Walk] · H(route clean, rails routes)[Build] · FunctionMeta[Walk] · StampOwners[Build] · TxScopes B[Walk] | none |
| swift | Query · H(bindings, calls)[Calls] · FieldReads · VariantRefs · H(text frameworks) · FunctionMeta[Walk] · H(member owners) · StampOwners[Build] | none |
| c | Query · H(calls)[Calls] · FieldReads · VariantRefs · H(indirect)[Walk] · H(named bindings)[Imports] · H(macro fallback)[Walk] · H(receiver owners)[Build] · FunctionMeta[Walk] · StampOwners[Build] | none |
| cpp | (tree: `PREPARE_SOURCE`[Walk], parse[Tree]) · H(override pre-scan)[Walk] · Query · SortDedupImports · H(lexical owners, ctor promotion, qualified owners)[Build] · H(bindings, calls)[Calls] · FieldReads · VariantRefs · H(indirect, text frameworks)[Walk] · H(macro fallback) · FunctionMeta[Walk] · StampOwners · H(friend restamp)[Build] | none |
| dart | Query · DedupNodes[Build] · H(calls)[Calls] · FieldReads · VariantRefs · H(text frameworks) · FunctionMeta[Walk] · StampOwners[Build] · TxScopes B[Walk] | none |

Sources for the lists: the `parse_prof::span` boundaries of each `parse_file` (Go `:425-1179`, Rust `:180-716`, TS `typescript/parser.rs:419-1042`, JS `javascript/parser.rs:185-847`, Python `python/parser.rs:953-1820`, Java `java/parser.rs:179-600`, Kotlin `kotlin/parser.rs:312-715`, C# `c_sharp/parser.rs:287-812`, PHP `php/parser.rs:489-1139`, Ruby `ruby/parser.rs:366-1022`, Swift `swift/parser.rs:163-671`, C `c/parser.rs:472-831`, C++ `cpp/parser.rs:316-890`, Dart `dart/parser.rs:176-559`).

Eleven languages fit with no move. Three languages (Python, Java, Kotlin) move one bind step. Section 4 shows that the move changes no output.

A neutral step may follow a bind step. Go runs `EventTopics` after `TxScopes`, and TS runs `SchemaFields` and `EventTopics` after `TxScopes`. They read the tree, the query or tap rows and the imports only, so the check allows them in their current place.

## 4. Binding

### 4.1 Where binding happens today

| Binding | Where | When |
|---|---|---|
| `RawTxScope` raw index, in the provider | Go `go/parser.rs:1161` (`:348`); Rust `rust/parser.rs:713-714`; Python `python/parser.rs:1677`; Java `java/parser.rs:577-578`; Kotlin `kotlin/parser.rs:695-698`; C# `c_sharp/parser.rs:808-811`; PHP `php/parser.rs:1137-1138`; TS `typescript/parser.rs:1011-1012`; Ruby `ruby/parser.rs:1020`; Dart `dart/parser.rs:558`. JS, Swift, C and C++ emit none. | inside `parse_file` |
| endpoints of routes, path literals, SQL refs, schema fields, event topics | `pipeline.rs:414-420` calls `LocalGraph::bind_records` (`ecp-core/src/analyzer/types.rs:1530-1546`) | after the provider returns, so after the node list is final |
| names (`source_name`, `enclosing_symbol`) | `enclosing_function_name` (Go `:1142`), Rust `fn_spans` (`:640-643`) | inside `parse_file`. C4 allows a name in `CollectCaptures`; it is not a raw index. |

C4 names Python only (`python/parser.rs:1430` on the old pin). The current pin shows three bind steps before a node-changing step:

| Lang | Bind | Node-changing steps after it |
|---|---|---|
| python | `resolve_tx_scopes` `:1677` | Class to Interface promotion `:1683-1687`; enum variants `:1693-1727`; owner stamps `:1729-1736` |
| java | `collect_jvm_transactional_scopes` `:577-578` | `stamp_owner_class_by_span` `:580` |
| kotlin | `collect_jvm_transactional_scopes` `:695-698` | `stamp_owner_class_by_span` `:700` |

### 4.2 Where the driver puts it

`Op::TxScopes` is a bind step. `steps_ok` rejects a node-changing step after it, so the step can only come after the last node change. `bind_records` stays in the pipeline: it serves every provider, including the SFC and secondary ones.

### 4.3 Which languages change, and the output effect

T2 changes no binding: Go binds after its last node push (`:1037-1043`), and Rust binds after `clear_scoped_owners` (`:708-714`). T4 moves Python. T5 moves Java and Kotlin.

The move changes no output, for these reasons:

- The tx binders read four things of a callable: its index, kind, span and decorators (`framework_helpers.rs:201-213`, `:859-873`).
- `stamp_owner_class_by_span`, `stamp_owner_fn_by_span` and `clear_scoped_owners` write `owner_class` only (`framework_helpers.rs:344`, `:448`; `lexical.rs:66`).
- The Python promotion changes `Class` to `Interface` (`python/parser.rs:1684-1686`). Neither kind is a callable.
- `process_class_for_enum` appends new nodes at the end (`python/parser.rs:802`), so earlier indexes stay. It retags only a node whose name and span equal an enum-body assignment (`:795-800`). A Python tx site is the name of a decorated `def` (`:1846-1850`). A lambda cannot hold a `def`, so no retagged node can enclose a site.

This is a source argument, not an executed proof for malformed trees. The dump proves it in T4 and T5, with a decorated nested function inside an enum class and inside a protocol class among the fixtures. A dump difference makes the move a capability change. It then leaves the extraction slice, gets its own slice, and bumps `BUILDER_FINGERPRINT`.

## 5. Performance

### 5.1 Generics and dynamic dispatch

| Place | Form | Why |
|---|---|---|
| `parse::<S>`, `Compiled<S>`, `S::capture`, `S::end_match`, `S::hook` | generic, monomorphised per language | The match loop runs per capture. A static call can inline. |
| `Entry<S::Role>` table | a dense `Box<[Entry]>` indexed by `cap.index` | One load and one `match` replace an if-chain of up to 25 `Option<u32>` compares. |
| `LanguageProvider` | `Box<dyn LanguageProvider>`, unchanged | One call per file exists today. |
| `TxRule` function, `PREPARE_SOURCE`, `POST_TREE` | `fn` pointers, once per file | Cold. |
| `EventTopicConfig.direction_classifier`, `SchemaFieldConfig.type_classifier`, `NameRefSpec.passed_callable` | `fn` pointers, unchanged | They exist today. |

The driver adds no `dyn` call. `node::decl` takes the owned `String` that the copies allocate today, and `Vec::new()` does not allocate. The pure moves add no heap allocation. The tap commit adds one: the tap-row buffer of §2.5, at most one row of 64 bytes or less per config per tapped match, freed per file.

### 5.2 Places that can cost time

Check each row with `ECP_PROF=1` per-pass totals for the Go and Rust files of the corpus, before and after T2. Check the normal (unprofiled) wall time too.

| # | Place | Pass | Expected | What to watch |
|---|---|---|---|---|
| 1 | table dispatch instead of the if-chain | Query | equal or lower | Go also drops about 25 `capture_index_for_name` scans per file. |
| 2 | tap range read per capture | Query | about one load per capture | a rise on files with many captures |
| 3 | tap slot writes and row push on a tapped match | Query | only matches with tapped captures | files with many event calls, with and without an active gate |
| 4 | the second event query run is gone | Walk | lower on files that pass an event gate | Rust and Go files that import Kafka, Redis, RabbitMQ or SQS |
| 5 | `Match` value per match | Query | equal | Pass `&mut Match` and reset it in place. A by-value move copies a few hundred bytes per match in Go. |
| 6 | `subtree_has_kind` with a `TreeCursor` instead of Rust's stack `Vec` (`rust/parser.rs:47-59`) | Query | lower for Rust | none |
| 7 | `FxHashSet` for the anonymous dedupe instead of the std `HashSet` | Query | lower | none; the hasher does not change output |
| 8 | the step loop and more spans | all | about 4 more spans per file | Each disabled span costs one relaxed atomic load (`ecp-core/src/analyzer/parse_prof.rs:51-52`, read at `:62`) and one branch on drop (`:77`). The cost is small but not zero, so compare the normal wall time as well as the per-pass totals. |
| 9 | `FileCx` field access through `&mut` instead of locals | all | within noise | Calls and Walk on large files |
| 10 | monomorphised code size | total | within noise | the wall time of a full cold index |
| 11 | tap rows of a gated-out file | Query | a small rise | peak RSS; the rows are dropped at the step |

Binary layout moves wall time by a few percent. Confirm a claimed gain or loss with the canonical bench (`python scripts/benchmark/benchmark_ecp.py`) and a control binary.

### 5.3 Acceptance rule for the tap commit

The tap commit lands only if it does not regress against the commit before it (the Go move). Measure both release binaries on the Go, Go-binary and Rust directories of the corpus. Use the same machine and run them serially:

```text
ECP_NO_CACHE=1 ECP_PROF=1 /usr/bin/time -v <ecp> admin index --force --repo <dir>
ECP_NO_CACHE=1 /usr/bin/time -v <ecp> admin index --force --repo <dir>
```

`ECP_NO_CACHE` turns off the parse cache (`ecp-cli/src/commands/admin/index.rs:217-218`), so every file parses.

| Metric | Source | Regression when |
|---|---|---|
| Query + Walk per-pass totals, Go and Rust providers | the `ECP_PROF` per-pass line | the median of 5 runs after exceeds the maximum of 5 runs before |
| peak RSS | `Maximum resident set size` of `/usr/bin/time -v` | the same rule |
| normal wall time | `Elapsed (wall clock) time`, unprofiled runs | the same rule |

Write the 10 numbers per metric into the commit message. If any metric regresses, drop the commit. The `Requery` ops then stay, and the one-query target also lists event topics and schema fields as open.

## 6. T2 implementation plan

### 6.1 Files

| File | Action | Commit | Predicted lines | Content |
|---|---|---|---|---|
| `crates/ecp-analyzer/src/driver/mod.rs` | create | 1 | 280 | `ParseSpec`, `KindCapture`, `Step`, `Op`, `Effect`, `TxRule`, `steps_ok`, `Compiled` with the overlap error, `parse` with the tree phase, the query loop, the step loop, the debug node-count assertion |
| `crates/ecp-analyzer/src/driver/cx.rs` | create | 1 | 190 | `FileCx`, `DeclSlot` with the three `KindCapture` writes, `TestRule`, `ScopeLanguage`, the two anonymous dedupe sets, the `LocalGraph` assembly |
| `crates/ecp-analyzer/src/driver/node.rs` | create | 1 | 150 | `decl`, `anonymous`, `span`, `span_from`, `subtree_has_kind`, `enclosing_or_module`, `dedup_consecutive`, unit tests |
| `crates/ecp-analyzer/src/driver/imports.rs` | create | 1 | 90 | `raw` with `binding_kind`, `ImportOrder::{SourceName, NameSourceAlias}`, unit tests |
| `crates/ecp-analyzer/src/driver/records.rs` | create | 1 | 160 | the `TxScopes` op, `EventTopicsRequery`, `SchemaFieldsRequery`, unit tests |
| `crates/ecp-analyzer/src/lib.rs` | edit | 1 | +1 | `pub mod driver;` |
| `crates/ecp-analyzer/src/rust/parser.rs` | edit | 2, 4 | 884 to about 700 | `impl ParseSpec for RustSpec`; `RustProvider` holds `Compiled<RustSpec>` |
| `crates/ecp-analyzer/src/go/parser.rs` | edit | 3, 4 | 1203 to about 930 | `impl ParseSpec for GoSpec`; `GoProvider` holds `Compiled<GoSpec>` |
| `crates/ecp-analyzer/src/driver/tap.rs` | create | 4 | 260 | `TapTable`, `TapTarget`, `TapRow`, the per-match slot table, `Op::EventTopics`, `Op::SchemaFields`, unit tests |

T2 adds about 1130 driver lines and removes about 460 lines from Go and Rust. The net line count falls only from T3 on, when each language reuses the driver.

`lib.rs` is not in the T2 file list of the slice plan. The module cannot compile without it. Approve the one-line edit with T2.

### 6.2 Steps and commits

Sub-agents edit only. The main session runs every cargo command, serially, with `-j 4`, after the editing agent returns.

| # | Who | Step | Commit |
|---|---|---|---|
| 0 | main session | Build at `87f2d860`. Take the baseline dump of §6.3 twice. `cmp` each pair of files: they must be equal. Keep one set as the baseline. | none |
| 1 | implementer | `driver/node.rs`, `driver/imports.rs`, `driver/cx.rs`, `driver/records.rs`, `driver/mod.rs`, `lib.rs`, with their unit tests. Build every `Op` variant except `EventTopics` and `SchemaFields`, and every spec parameter of §2: `KIND_CAPTURE` with all three variants, `BLIND` for unconditional captures, both `AnonKey` sets, `imports::raw` with `binding_kind`, both `ImportOrder` keys, `DedupNodes`, `TxRule::Nodes` with `kinds`, `TxRule::Tree`, `TxRule::Sites`, `PREPARE_SOURCE`, `POST_TREE`, `SCHEMA_FIELDS`, the overlap error and the phase checks. T3 to T7 may not edit the driver. | 1: `feat(driver): shared parse driver with no caller` |
| 2 | main session | Build, test, clippy. | — |
| 3 | implementer | Rust onto the driver, with `Op::EventTopicsRequery`. Rust first: it has fewer side tables and no tree nodes in its file state. | 2: `refactor(rust): parse through the shared driver` |
| 4 | main session | Build, test, the dump gate of §6.3. | — |
| 5 | implementer | Go onto the driver, with `Op::EventTopicsRequery`. | 3: `refactor(go): parse through the shared driver` |
| 6 | main session | Build, test, the dump gate. The perf comparison of §5.2 against `87f2d860` for rows 1, 2, 5-10. | — |
| 7 | implementer | `driver/tap.rs`; `Op::EventTopics` and `Op::SchemaFields` in `driver/mod.rs`; Go and Rust switch to `Op::EventTopics`. | 4: `perf(driver): one query pass feeds event topics and schema fields` |
| 8 | main session | Build, test, the dump gate, the rule of §5.3 against commit 3. Drop commit 4 if the rule fails. | — |

**Outcome of step 8 (2026-10-30).** Commit 4 passed the dump gate and its 18 tap tests, and failed the §5.3 rule: Rust peak RSS median 238 048 KB against a commit-3 maximum of 236 356 KB, in the second of two 5-run A/Bs. Every per-pass time and the unprofiled wall time passed. The Go and Rust corpus holds no event-import file, so the re-query never ran and the tap's gain was unmeasurable there. Commit 4 is dropped. Its patch is `.claude/salvaged-patches/2026-10-30-p3-t2c4-event-schema-tap.patch` (gitignored, local). The tap returns as its own commit after T3, with TypeScript switched to `Op::SchemaFields`: TypeScript re-runs its query for schema fields on every file, so the gain is measurable on the TypeScript corpus. That commit is a driver change and passes the same dump gate and §5.3 rule against the commit before it.

### 6.3 Tests and the pass criterion

| Test | Commit | Asserts |
|---|---|---|
| `test_decl_sets_empty_fields_and_hash` | 1 | every default field and the hash |
| `test_anonymous_name_uses_row_plus_one` | 1 | the name format |
| `test_subtree_has_kind_root_itself_returns_true` | 1 | the root counts |
| `test_subtree_has_kind_absent_returns_false` | 1 | a tree with no such kind |
| `test_import_raw_keeps_binding_kind` | 1 | `Some(Alias)`, `Some(Macro)` and `None` pass through unchanged |
| `test_import_order_source_name_keeps_first_alias` | 1 | stable sort, then dedupe; the first alias survives |
| `test_import_order_name_source_alias_keeps_distinct_aliases` | 1 | two imports that differ only in alias both stay |
| `test_dedup_nodes_separated_duplicate_stays` | 1 | `A, B, A` keeps three nodes; `A, A, B` keeps two |
| `test_decl_slot_name_keep_first_keeps_first_kind` | 1 | Rust semantics |
| `test_decl_slot_root_overwrite_sets_root_not_name` | 1 | Ruby semantics; `name` stays `None` |
| `test_compiled_capture_in_two_slots_returns_err` | 1 | the overlap error |
| `test_steps_ok_node_step_after_bind_returns_false` | 1 | the phase check |
| `test_steps_ok_tx_not_bind_returns_false` | 1 | the fixed-op effect check |
| existing `parse_prof_coverage.rs` | 2, 3 | Tree and Query time for Go and Rust; a zero-byte file parses (`crates/ecp-analyzer/tests/parse_prof_coverage.rs:131-149`) |
| existing `go_kafka_events.rs`, `go_redis_events.rs`, `go_rabbitmq_events.rs`, `go_events_sqs.rs`, `rust_kafka_events.rs`, `rust_redis_events.rs`, `rust_rabbitmq_events.rs`, `rust_events_sqs.rs` | 2, 3, 4 | all 8 Go and Rust event configs, end to end through `parse_file` |
| `test_event_tap_<config>_equals_extract_event_topics`, one per Go and Rust config (8 tests) | 4 | tap output equals `extract_event_topics` on the same tree, gate active |
| `test_event_tap_sqs_rust_empty_producer_emits_empty_enclosing_fn` | 4 | `enclosing_fn == ""`, record present |
| `test_event_tap_no_active_gate_returns_none_and_drops_rows` | 4 | `event_topics` is `None`; the family's rows are gone after the step |
| `test_event_tap_two_configs_one_match_emits_once` | 4 | the match-group boundary and config order |
| `test_event_tap_repeated_capture_last_write_wins` | 4 | the later capture's text |
| `test_event_tap_invalid_utf8_topic_emits_empty_topic` | 4 | `topic_literal == Some("")` |
| `test_schema_tap_ts_interface_empty_gate_is_active` | 4 | TS interface fields with no import, equal to `extract_schema_fields` |
| `test_schema_tap_pydantic_equals_extract_schema_fields`, `test_schema_tap_sqlalchemy_equals_extract_schema_fields` | 4 | both Python configs, gate active |
| `test_schema_tap_owner_without_name_emits_nothing` | 4 | the two required slots |
| `test_schema_tap_python_no_import_returns_none` | 4 | the import gate of a non-empty gate |

The tap tests build their tree and query the way each provider does. The Python and TS schema tests do not need a Python or TS spec: they feed the tap from a plain `QueryCursor` loop over the query that `PythonProvider::new` and `TypeScriptProvider::new` compile (`crates/ecp-analyzer/tests/schema_field_extract.rs:15-26` shows the tree setup).

Edge cases the implementer covers: an empty file; a file with imports only; one closure in two matches; a Rust `impl` with no self type (the early return at `rust/parser.rs:387` also skips the import push of that match, but not the tap row); a name that is not UTF-8 (the copies skip it); a gated and an ungated event file; a `_` Go var name.

**The dump.** Run the ignored dump test once per top-level directory of `/home/enor/code-graph-nexus/.sample_repo/`, with one output file per directory. On 2026-10-30 there are 23 directories: `C`, `CSharp`, `Cpp`, `Dart`, `Go`, `Go-binary`, `Java`, `JavaScript`, `Kotlin`, `PHP`, `Python`, `Ruby`, `Rust`, `Swift`, `Swift-cli`, `TypeScript`, `bash`, `crystal`, `dockerfile`, `lua`, `move`, `solidity`, `zig`.

```text
ECP_DUMP_ROOT=/home/enor/code-graph-nexus/.sample_repo/<dir> ECP_DUMP_OUT=<out>/<dir>.txt \
  cargo test -p egent-code-plexus --test local_graph_dump -j 4 -- --ignored --test-threads=4
```

Each file is path-sorted and order-sensitive inside each graph (`local_graph_dump.rs:37-41`). The test asserts only that some graph exists (`:32-36`). So also record the count of `LocalGraph {` entries per file. Check that each migrated language's directory holds at least one graph of that language.

**Pass criterion, written before the run.** The baseline at `87f2d860` is deterministic: two runs give equal files. For each slice T2 to T7:

- The dump of each directory that holds the slice's languages equals the baseline.
- The dump of every other directory equals the baseline too. A file of one language can sit in another language's directory.
- The `LocalGraph {` count of each file equals the baseline count.
- `BUILDER_FINGERPRINT` (`ecp-core/src/registry/commit_meta.rs:166`) is unchanged.

One differing line fails the slice.

Name what the corpus cannot show before the run. Check that the corpus holds Go gin, echo and route groups; Go `iota` blocks; typed and untyped file vars; Rust trait impls with attribute patterns; Axum and Actix routes; nested `use` lists; `#[transaction]`; and an event import for each of the 8 Go and Rust configs. A config with no corpus file is proven only by its tap unit test and its existing end-to-end test.

### 6.4 Risks

| Risk | Control |
|---|---|
| A per-match push moves inside one match (Go emits fields, vars, file vars, locals, the declaration, the route, the import, in that order: `:696-1028`). | `end_match` keeps the statement order. The dump shows any slip. |
| `KIND_CAPTURE` differs (Go overwrite, Rust keep-first, Ruby root). | a spec constant; the `DeclSlot` unit tests; the dump |
| A capture name falls into two slots, so the arm order of today decides it. | the overlap error of `Compiled::new` |
| A conditional blind capture goes into `BLIND` and loses its predicate. | `BLIND` holds unconditional captures only; the table in §2.4 names every conditional one; the dump |
| Rust declaration merge by name, span and priority (`:470-519`) | stays spec code in `end_match` |
| The empty-to-`None` rule of a `LocalGraph` field differs per language. | The driver uses `(!v.is_empty()).then(…)` for every `Option<Box<[_]>>` field, as Go and Rust do (`go/parser.rs:1194-1196`, `rust/parser.rs:696-697`, `:732`). T3 to T7 check their own fields. |
| GAT lifetimes: Go keeps `tree_sitter::Node` values in file state (`route_arguments`, `:475`). | The tree stays local to `parse`; `type File<'t>` ties the nodes to its borrow. |
| Blind-spot order: Go pushes iota spots before the loop (`:442`). | `H(iota)` stays the first step. |
| A tap capture name equals a role capture name. | `Entry.tap` is independent of `Entry.slot`. |
| The tap retains rows for gated-out files. | the bound of §2.5 and the rule of §5.3 |
| A hook writes more than its declared effect. | §3.1: the debug node-count assertion, code review, the dump |
| The slice plan forbids driver edits in T3 to T7. | T2 builds every parameter and op of §2 and §6.5. A gap goes back to the driver owner as a reported change. |
| The five decorator tx functions (`framework_helpers.rs:859-980`) stay duplicated. | Not in any slice's file list. File a follow-up to merge them after T7. |

### 6.5 What each later slice needs from the driver

**T3, TypeScript and JavaScript.** TS creates a `Parser` per file (`typescript/parser.rs:415-417`); `parse_tree` keeps that, or T3 adds a thread-local as a separate perf change. TS reruns the whole query for schema fields on every file, because its one schema gate is empty. It reruns it again for its four event configs when an event gate holds (`:1018-1041`). T3 lands each spec first with `SchemaFieldsRequery` and `EventTopicsRequery`, then switches to the tap ops in a separate commit under the rule of §5.3. `TS_INTERFACE_CONFIG` has an empty gate, so the schema step treats it as active (§2.5). Both need the conditional blind roles of §2.4. Both need `stamp_js_ts_exports`, which returns `export_aliases` (`FileCx.export_aliases`), and `mark_js_ts_import_bindings`, which writes imports, as hooks. Both pass `js_ts_reexport_kind` as `binding_kind`. Both need `StampOwnerFns`, `ClearScopedOwners` with `ScopeLanguage::FromPath`, `cx.call_sites` for indirect dispatch, and route cleaning. JS needs `FileCx.route_receivers` (`javascript/parser.rs:759-763`), the shared span set of §2.4, and emits a bare `<anonymous>` name at `:336`, which stays spec code. NestJS refs use the class as their source (`typescript/parser.rs:970-977`).

**T4, Python and Ruby.** Python keeps its import pre-scan as a pre-query hook (§2.7). Python needs `TestRule::PythonPath`, `AnonKey::AnyNodeSpan`, `TxRule::Sites` moved to a bind step (§4), `FileCx.fanout_refs`, two schema configs and five event configs (`:1742-1769`) with the same requery-then-tap commit order as T3, and a last import-normalization hook that runs after the event and schema steps read the raw module names (`:1776-1819`). Ruby needs `KindCapture::RootOverwrite` and a `name` role (§2.2), the `blind.send` role (§2.4), a pre-query visibility walk (`ruby/parser.rs:372-374`), a node-changing hook that pushes delegator nodes and import bindings before the calls (`:908-939`), the Rails route blind spots (`:994-1001`) and `TxRule::Tree` after the stamp (`:1020`). T4 fixtures: a Ruby class, module, `initialize` constructor and constant; `obj.send(:to_s)` against `obj.send(name)`.

**T5, Java, Kotlin and C#.** Java and Kotlin keep a `node_id_to_idx` map for multi-capture merges (`java/parser.rs:194`, `kotlin/parser.rs:327`, `c_sharp/parser.rs:299`); it lives in `File<'t>`. Java needs `AnonKey::NodeId`. Kotlin needs `POST_TREE` for `recover_same_line_closers` (`kotlin/parser.rs:317-318`). Java and Kotlin move `TxScopes` after `StampOwners` (§4) and keep their own `kinds` lists (§2.4). C# needs `TxRule::Nodes` with the dotnet function and its `kinds`, and the text framework detector as a hook.

**T6, PHP, Swift and Dart.** PHP runs two call extractors and builds fan-out refs and blind spots from dynamic calls in one hook (`php/parser.rs:966-987`). PHP needs the `blind.call_user_func` role (§2.4). T6 fixtures: `call_user_func('handler')` against `call_user_func($f)`. Its Laravel route sites keep tree nodes in file state (`:509-515`) and read the imports and the first namespace (`:926-933`). After the stamp, PHP restamps const owners (`:1105-1131`) and adds qualified constant reads (`:1134`): both are node-changing steps before its bind step. Swift keeps its enum-case push order (§2.6), needs a member-owner hook before the generic stamp (`swift/parser.rs:669-670`) and has no tx step. Dart needs `Op::DedupNodes`, the consecutive `dedup_by` on name, span and kind before the calls (`dart/parser.rs:492`), and `TxRule::Tree` (`:558`).

**T7, C and C++.** C++ needs `PREPARE_SOURCE` for `blank_decl_prefix_macros` (`cpp/parser.rs:316-329`): the tree parses the patched bytes, and every later step reads the original bytes. C++ keeps its override pre-scan as a pre-query hook (§2.7). C++ needs `ImportOrder::NameSourceAlias` (`:722-730`) and node-changing hooks for lexical owners, constructor promotion and qualified owners (`:734-798`). Both C and C++ push macro-fallback nodes after the calls (`c/parser.rs:808`, `cpp/parser.rs:879`). That is allowed, because neither has a bind step. C adds named-binding imports after the calls (`c/parser.rs:797`), passes its `binding_kind` values through `imports::raw` (§2.4) and has no anonymous node.

## 7. Rejected alternatives

| Alternative | Reason |
|---|---|
| `dyn ParseSpec` or a struct of `fn` pointers for the capture callbacks | an indirect call per capture in the hot loop |
| fixed driver phases with hooks between them | Go, Rust, TS and Python order their steps differently, so each would need a reorder and a proof per language |
| helper-first extraction: shared node and import constructors and subtree predicates, while each parser keeps its own loop and statement order | It removes the repeated field setup with fewer moving parts and no `Step`, `Effect` or shared context. But the 14 query loops, capture tables, step orders and `LocalGraph` assemblies stay duplicated, so it does not meet the shared-driver target, and every later change still touches 14 loops. This design found no language where the driver cannot keep byte identity: each difference that the review named is a spec parameter, a role or a hook (§2.2-§2.7). The helpers of §2.4 land first inside commit 1 either way. |
| one shared role enum for all languages | couples every language to every capture name |
| a `macro_rules!` template for `parse_file` | hard to read and to debug; no type-checked spec boundary |
| sort the records in the dump | hides an order change that moves uids (§1) |
| unify the `.scm` capture names in T2 | changes the query, so match order and node order can change |
| merge the 14 `receiver_types.rs` copies in T2 | each copy computes a different language's binding |
| keep the second event query run as the end state | breaks the slice rule "one query pass feeds … event topics" |
| land the tap in the same commit as the Go and Rust moves | a dump difference or a perf loss could not be traced to the tap, and the tap could not be dropped alone |
| collapse Python's import pre-scan in T2 | T4 owns `python/parser.rs` |
| one driver-wide thread-local `Parser` map | a lookup per file and a shared mutable type, for no gain over one thread-local per language |
| move `bind_records` into the driver | the pipeline already runs it after every provider, including SFC and secondary providers (`pipeline.rs:414-420`) |
