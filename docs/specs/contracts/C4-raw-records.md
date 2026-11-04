# C4: raw records

Status: contract, phase 1. Types: `crates/ecp-core/src/contracts/raw_records.rs`. Landed: slice P5 (`RawIndexMap` replaces the per-pass prefix sums) and slice P6 (`PathLiteral`, `SqlRef`, `SchemaField` and `EventTopic` records carry a bound `endpoint`; a module-top site binds `File`). The section "Today" records the state before those slices.

Plan row (`docs/specs/2026-10-25-architecture-direction.md`, "Contracts before code"): the parser phases (collect captures, finalize nodes, bind raw records), the raw index to global node mapping, the endpoint of a raw record (node, file or unresolved), and the evidence classes exact, ambiguous, heuristic and unresolved. Consumers: areas 2, 4, 5 (the records the overlay replays), 7, 8, 10.

Every `file:line` below was read on branch `feat/arch-p1-contracts` at `f23bce5a`. Paths without a crate prefix are under `crates/ecp-analyzer/src/`.

## Defines

### Terms

| Term | Meaning |
|---|---|
| raw node | One `RawNode` in a file's `LocalGraph.nodes`. |
| raw record | A fact a parser emits beside the raw nodes: a path literal, a SQL reference, an event topic, a transaction scope, a route, a framework or fan-out reference, a call annotation, function metadata. |
| site | The source span where a parser found a record. |
| raw index | The position of a raw node in the file's final node list. It is file-local. |
| slot | A global node index in the raw range of the build or of the overlay. |
| endpoint | The node, the file, or the unresolved name that a record attaches to. |

### Types

| Type | Kind | Purpose |
|---|---|---|
| `ParsePhase` | enum | `CollectCaptures`, `FinalizeNodes`, `BindRecords`, in that order. |
| `PhaseError` | result | A transition that skips a phase or goes back. |
| `RawIdx` | stored `u32` | A raw index. `to_packed24` checks that it fits a 24-bit field (Open point 9). |
| `RawEndpoint` | stored enum | `Node(RawIdx)`, `File`, `Unresolved(Unbound)`. |
| `SourceName` | stored string | A source spelling, trimmed and not empty. Its field is private; `SourceName::new` is the only constructor outside the module. |
| `Unbound` | stored enum | `Named(SourceName)`, `Anonymous`, or `Inline(Span)`. |
| `EndpointError` | result | A `Node` out of range, or a name that is empty or not trimmed. |
| `Bound` | value | One endpoint, the evidence that bound it, and the same-name candidates that the site cannot be proven to see. |
| `Evidence` | enum | `Exact`, `Ambiguous { candidates }`, `Heuristic`, `Unresolved`. |
| `EvidenceError` | result | An evidence class that does not fit its endpoint or its edge. |
| `HEURISTIC_CEILING` | const `0.7` | The confidence that separates a guess from a fact. |
| `FileOrd` | `u32` | The position of a file in the build input (sorted by path) or in the overlay input. |
| `RawIndexMap` | value | The one map from `(FileOrd, RawIdx)` to a slot, and back. |
| `GlobalEndpoint` | value | `Node(slot)`, `File(FileOrd)` or `Unresolved(&Unbound)`. |
| `IndexMapError` | result | Slot overflow or a bad tombstone pair. |
| `RecordKind`, `EndpointRule` | enum | Each record kind, its binding rule and the edge kinds it yields. |

### Parser phases

| Phase | May change nodes | May write a `RawIdx` | Rule |
|---|---|---|---|
| `CollectCaptures` | yes | no | A record keeps its site, or the name the source writes. |
| `FinalizeNodes` | yes | no | Dedup, reorder, kind promotion and owner stamping happen here. After it, the count, order, kind and span of every node are fixed. |
| `BindRecords` | no | yes | Each record gets a `RawEndpoint` by its `RecordKind::endpoint_rule`. |

`ParsePhase::advance` allows only the next phase. Code that concatenates two node lists (an SFC with several `<script>` blocks) adds the length of the first list to every `RawIdx` of the second.

### Endpoint and binding

| Item | Invariant |
|---|---|
| `RawEndpoint::Node(i)` | `i` is below the node count of the record's own file (`validate`). |
| `RawEndpoint::File` | The record belongs to the whole file, or sits at module top level. The builder maps it to the file's `File` node. |
| `Unbound::Named(s)` | `s` is the trimmed source spelling and is not empty. `SourceName::new` trims and returns `None` for a blank name. `RawEndpoint::named` and `bind_name` build it only through `SourceName::new`. `validate` and `Bound::new` reject a value that breaks the rule, such as one read back from an archive. A later pass can bind it across files. |
| `Unbound::Anonymous` | No name: the producer saw an empty or whitespace-only reference. The line comes from the record's site. |
| `Unbound::Inline(Span)` | A function written in place as a route's handler argument, at the argument's span. `RawRoute::bind` takes only the outermost closure node that starts at that span, so a middleware closure of the same registration never serves the route; with no such node the handler stays `Inline`. |
| `bind_enclosing(nodes, site, accept)` | The innermost node of an accepted kind whose span contains the site. Between identical spans the lower index wins, as in `innermost_enclosing` (`ecp-core/src/analyzer/types.rs:40`). |
| `bind_name(nodes, site, name, accept)` | A same-name node of an accepted kind is a candidate only when the site provably sees it. Every scope container around the candidate (callable, type, `Impl`, `Module`, `Namespace`) must also contain the site, and none of them may be a type scope. Member visibility (`self.`, an implicit `this`) belongs to area 1. Among the visible candidates, the innermost scope shadows the outer ones. In that scope: 1 match gives `Node`, `Exact`. 2 or more give `Unresolved(Named)`, `Ambiguous` with the true count. 0 gives `Unresolved(Named)`, `Unresolved`. An empty name gives `Unresolved(Anonymous)`, `Unresolved`. A candidate that the site cannot be proven to see never binds. `Bound::out_of_scope` keeps its raw index, so a caller can record a `BlindSpot` or a caveat. The rule ignores declaration order: a declaration after the site in a visible scope counts. It never picks the first of several. |
| `Bound::new` | `Node` takes `Exact` or `Heuristic`. `File` takes `Exact`. `Unresolved` takes `Ambiguous` (2 or more) or `Unresolved`. A `Named` endpoint with a blank or untrimmed name is `BadName`. |

### Evidence classes

| Class | Meaning | Edge confidence | Edge `RelType` |
|---|---|---|---|
| `Exact` | Found by structure (a raw index, the declaration's own record) or by one unique scoped match. | `[0.7, 1.0]` | not heuristic by kind |
| `Ambiguous { candidates }` | 2 or more candidates match equally. None is picked. | `(0, 0.7)` | any |
| `Heuristic` | Picked by a rule that can be wrong: a repo-unique basename, path proximity, a name-and-type mirror, a templated URL. | `(0, 0.7)`, or any value in `(0, 1]` when `RelType::is_heuristic` (`ecp-core/src/graph.rs:147-152`) | any |
| `Unresolved` | No candidate. | no edge | — |

`Evidence::check(confidence, rel)` enforces the table. `Evidence::of_stored(confidence, rel)` reads a stored edge back as `Exact` or `Heuristic` (Open point 1). A `Heuristic` edge carries a `reason` that differs from the `Exact` reason of the same producer.

### Raw index map

| Function | Rule |
|---|---|
| `RawIndexMap::new(base, node_counts)` | Slots `base .. end()` hold every raw node of every file, file after file, in input order. The full build uses `base = 0`. The overlay uses the base graph's node count. `Overflow` when a slot does not fit in `u32`. |
| `with_merged(pairs)` | `(tombstone, survivor)` slots from pass 1. The survivor comes first and is never a tombstone. |
| `slot(file, raw)` | The raw node's own slot, tombstone or not. |
| `node(file, raw)` | The slot that owns the identity: the survivor for a tombstone. An edge never starts or ends at a tombstone. |
| `locate(slot)` | The inverse of `slot`. An empty file is skipped. |
| `resolve(file, endpoint)` | `GlobalEndpoint`. A `File` endpoint stays `File(FileOrd)`; the builder fixes the `File` node index (Open point 3). |

The map reads node counts only, never uids. A tombstone keeps its slot. Nodes that the builder makes later (routes, entry points, detector nodes, processes, `File` nodes) sit at `end()` and above.

### Record kinds

| `RecordKind` | Today's type | Endpoint rule | Accepted kinds | File fallback | Edge kinds |
|---|---|---|---|---|---|
| `Import` | `RawImport` | `FileOnly` | — | — | `Imports` |
| `Route` | `RawRoute` (call form) | `Named` (the handler) | callable | — | `HandlesRoute` |
| `FrameworkRef` | `RawFrameworkRef` | `Enclosing` | callable, type | yes | `References` |
| `FanoutRef` | `RawFanoutRef` | `Enclosing` | callable | yes | `References` |
| `PathLiteral` | `RawPathLiteral` | `Enclosing` | callable, `Const` | yes | `UsesPathLiteral` |
| `SqlRef` | `RawSqlRef` | `Enclosing` | callable, `Const` | yes | `QueriesTable` |
| `SchemaField` | `RawSchemaField` | `Enclosing` | type | no | `HasProperty`, `MirrorsField` |
| `EventTopic` | `RawEventTopic` | `Enclosing` | callable | yes | `Publishes`, `Subscribes`, `EventTopicMirror` |
| `TxScope` | `RawTxScope` | `Enclosing` | callable | no | `OpensTxScope` |
| `CallMeta` | `RawCallMeta` | `Enclosing` | callable | no | `Calls` |
| `FunctionMeta` | `RawFunctionMeta` | `Enclosing` | callable | no | none |
| `FetchSite` | none (Open point 4; the overlay caveat applies until it exists) | `FileOnly` | — | — | `Fetches` |

Callable is `Function`, `Method`, `Constructor`. Type is `Class`, `Struct`, `Interface`, `Trait`. With no container and no file fallback, `RecordKind::bind_site` returns `None`: the record has no owner, and the producer drops it with a count, never with a guess. Facts inside a `RawNode` (`calls`, `field_reads`, `heritage`, `decorators`) start at that node by construction and need no endpoint.

### Scope table and import bindings

Status: contract for a future slice. No parser emits these records, and no type below exists yet. "Import scope today" records the current state. The plan's "Order" section places the slice. Every `file:line` in this section and in "Import scope today" was read at `56b7dbae`.

Declarations and imports share one scope model. A parser emits a scope table per file, and each import row names its scope occurrence. The `Imports` edge keeps its `FileOnly` endpoint: the edge source and the binding scope are two facts. The existing type `ImportScope` (`ecp-core/src/session/import_scope.rs:167`) holds the overlay's module-discovery facts, and it is not this scope.

| Term | Meaning |
|---|---|
| scope occurrence | One source region where names bind: the file, a module, a namespace block, a type body, a callable body, or an anonymous block. |
| scope table | The scope occurrences of one file, in pre-order. |
| import binding | One `RawImport` row, read with its scope and its site. |
| target recipe | The fields that say what an import binding names: `source`, `imported_name`, `alias`, `binding_kind`, as today. |
| in reach | Visible at a use site under the language's lookup rule. |
| opaque binding | An import binding in reach whose target recipe resolves to no declaration in the graph: a missing module, an external package, or an unknown target. |

| Type (planned, `ecp-core/src/contracts/raw_records.rs`) | Kind | Purpose |
|---|---|---|
| `ScopeId` | stored `u32` | The position of an occurrence in its file's scope table. `ScopeId(0)` is the file. C1 "Scope occurrence identity" fixes its stability. |
| `ScopeKind` | stored enum | `File`, `Module`, `Namespace`, `Type`, `Callable`, `Block`. |
| `RawScopeOccurrence` | stored record | `parent: Option<ScopeId>`, `kind`, `extent: Span`, `decl: Option<RawIdx>` (the declaring node; C1 derives its anchor). The name keeps it apart from C1's `Scope`, which is part of a declaration key. |
| `ScopeError` | result | An occurrence or a binding that breaks an invariant below. |

| Record | New field | Meaning |
|---|---|---|
| `LocalGraph` | `scopes: Vec<RawScopeOccurrence>` | The file's scope table. |
| `RawImport` | `scope: ScopeId` | The occurrence that the binding belongs to. |
| `RawImport` | `site: Span` | The span of the import clause. |

The local name and the form (named, wildcard, module) of a binding come from its target recipe, by the rule that the import tiers use today (`reexport::named_import`, `ecp-core/src/analyzer/reexport.rs:1040`). The lookup index holds them typed. They are not stored, because a stored copy is a second spelling that can drift from that rule.

| Item | Invariant |
|---|---|
| Root | `scopes[0]` is the only `File` occurrence. It has no parent, and its extent is the whole file. |
| Parent | Every other occurrence has a parent with a lower `ScopeId`. So the parent chain is acyclic and ends at the root. |
| Nesting | A parent's extent contains the extent of each child. Two sibling extents do not overlap. |
| One region | A callable body or a type body is one occurrence. The producer does not also emit a `Block` with the same extent. |
| `decl` | `Some(i)`: raw node `i` passes `is_scope_container` (C1), and its kind fits `kind` (next table). `File` and `Block` take `None`. A container with no node takes `None`, such as a TS `namespace` today (C1 Open point 14). |
| Binding scope | The `site` of an import binding lies inside the extent of its `scope`, and no child of that scope contains the site. |
| Declaration scope | A declaration belongs to the innermost occurrence that contains its span, its own body excluded. It needs no stored field. |
| Dedup | An import dedup key includes `scope`. Two rows with one target recipe in two scopes stay two rows. |
| Phases | The producer fixes the table order before it writes a `ScopeId`. `decl` binds in `BindRecords`, as a `RawIdx` does. |
| Concatenation | Code that joins two raw graphs adds the length of the first table to every `ScopeId` of the second. The second root then takes kind `Block` and the first root as parent. |

| `ScopeKind` | `decl` kinds |
|---|---|
| `Module` | `Module` |
| `Namespace` | `Namespace` |
| `Type` | `Class`, `Struct`, `Enum`, `Interface`, `Trait`, `Impl` |
| `Callable` | `Function`, `Method`, `Constructor` |
| `File`, `Block` | none |

**Scope lookup.** Area 1 owns the visibility policy. C4 fixes its input and its result.

| Rule | Statement |
|---|---|
| Input | The file, the use site, the name and the target kind. The caller's span is not the input, because two sibling blocks of one callable differ only by site. |
| Walk | The walk starts at the innermost occurrence that contains the use site and ends at the root. At each occurrence, the candidates are its declarations and its import bindings. |
| Language rule | A per-language rule decides three things: whether a binding is visible before its site, which candidate wins inside one occurrence, and which occurrences the walk skips (a Python class body, seen from a nested callable). |
| Stop | The first occurrence with a candidate in reach ends the walk. An outer occurrence never overrides it. |
| Replaces | For a language whose parser emits a scope table, the walk replaces Tier 1 (`same_file`) and the named-import tier for a bare name. Every other language keeps today's tiers. |
| Cost | The index of occurrences and names is built at parse or refresh time. A query reads it and never walks the AST. |

**Lookup outcome.** The walk adds one reason to `NoEdge` (`ecp-core/src/analyzer/outcome.rs:87-103`) and one row to the fallback table of shared-resolution §3.2. The row lives here: §3.2 records the design state at `ff75103a`, and this contract is the current rule.

| Walk result | `Resolution` | Later ladder tiers | Kotlin retry | Constructor fallback | Outer chain level |
|---|---|---|---|---|---|
| One candidate in reach, and its target resolves | `Bound(hit)` | as §3.2 row `Bound` | as that row | as that row | as that row |
| An opaque binding in reach | `NoEdge(OpaqueBinding)`, `reach()` is `Call` | skip | skip | skip | `NoEdge(ChainBroken)` |
| No candidate in reach, and a declaration of the name elsewhere in the file | `Shadowed` | as §3.2 row `Shadowed` | as that row | as that row | as that row |
| No candidate in the file | `Unknown` | run | runs | runs | `Unknown`, then the level fallback |

`OpaqueBinding` has the `Call` reach, because a later tier or a fallback binds a namesake that the binding hides: `global` runs last in `name_tiers` (`ecp-core/src/analyzer/name_tiers.rs:515`). Its evidence class is `Unresolved`: no edge. `NoEdge::External` keeps its `Ladder` reach (`ecp-core/src/analyzer/outcome.rs:106-111`) for the import-member tiers that use it today.

**Stored surfaces.**

| Surface | Must carry | Identifier |
|---|---|---|
| parse cache (`LocalGraph`) | `scopes`, and `scope` and `site` on each `RawImport` (`ecp-core/src/analyzer/types.rs:659-672`, `LocalGraph.imports` at `:1456`) | bump `BUILDER_FINGERPRINT` (`ecp-core/src/registry/commit_meta.rs:190`, now `schema52`) |
| L1 fragment | `FragmentFile.imports` is a `Vec<RawImport>` (`ecp-cli/src/session/overlay_writer.rs:148`), so the binding fields travel with it. `FragmentFile` gains `scopes`. | a new fragment format value. The reader accepts one value (`ecp-cli/src/session/overlay_reader.rs:206`). V7 shipped in v0.17.0, so the new fields do not extend it in place. |
| `graph.bin` | `ImportFact` (`ecp-core/src/graph.rs:855-867`) gains `scope` and `site`. `ZeroCopyGraph` gains every file's scope table, in CSR form as `file_imports` (`graph.rs:1319-1320`). The overlay reads a clean module's imports from these facts (`graph.rs:850-852`). | bump `GRAPH_FORMAT_VERSION` (`graph.rs:47`, now 20) |
| overlay memo and view cache | The named-import memo key adds the occurrence to `(file_ord, target)` (`ecp-core/src/session/view/resolution.rs:2842`). A `NamedImport.position` (`ecp-core/src/analyzer/reexport.rs:1026`) indexes the list it was computed on (`ecp-core/src/analyzer/name_tiers.rs:375`), so one key never serves two lists. | `VIEW_CACHE_FORMAT` follows the above |

**Test matrix.** The dimension is `import_scope`. Each case asserts the raw records, the full-build target, the overlay target, and the edges that must not exist. A full-build and overlay match alone is no proof: both sides can bind the same wrong target.

| Language | Test file | Occurrences that hold an import | Round |
|---|---|---|---|
| Rust | `crates/ecp-analyzer/tests/rust_import_scope.rs` | module, callable body, block | 1 |
| Python | `crates/ecp-analyzer/tests/python_import_scope.rs` | callable body, type body | 1 |
| C# | `crates/ecp-analyzer/tests/csharp_import_scope.rs` | namespace block | 1 |
| PHP | `crates/ecp-analyzer/tests/php_import_scope.rs` | namespace section, bracketed or not | 1 |
| C++ | `crates/ecp-analyzer/tests/cpp_import_scope.rs` | namespace, block; type body for a using-declaration | 2 |
| TypeScript | `crates/ecp-analyzer/tests/typescript_import_scope.rs` | namespace (`import X = A.B`), ambient `declare module` body | 2 |

| Case group | Cases |
|---|---|
| Reach | file, namespace or module, callable body, anonymous block |
| Isolation | sibling occurrences that bind `g` to two targets; no reach outside the occurrence; a namespace reopened in one file |
| Precedence | an inner import against an outer declaration; an inner declaration against an outer import; a local or parameter of the same name |
| Order | a use before and after the binding; a hoisted item against a statement-order binding |
| Identity | inserted whitespace; an inserted unrelated occurrence; swapped sibling blocks |
| Failure | a missing module, an external target, an ambiguous target, unsupported or malformed syntax: no fallback edge |
| Cache | one file, one name, one target kind, two occurrences; query A then B, and B then A, give one result |
| Overlay | a changed import source, alias or occurrence; a deleted or moved import; a changed dependency export with the caller's bytes unchanged |

Contract tests in `raw_records.rs`: an acyclic parent chain, each binding inside its scope, a round trip through the parse cache, the L1 fragment and `graph.bin` that keeps every occurrence, a dedup that keeps sibling rows, and a lookup result that does not depend on query order.

The other eight mainstream languages get an exemption line `import_scope <lang> <reason>` in `crates/ecp-analyzer/tests/lang_dimension_exemptions.txt`:

| Language | Reason |
|---|---|
| Java | An import applies to the whole compilation unit (JLS §7.5). |
| Kotlin | The import list is at the file head only. |
| Go | An import name belongs to the file block. |
| Swift | An import is at file scope only. |
| Dart | An import is a library directive. |
| JavaScript | A static import is a module item. A `require` or `import()` result is a dynamic load binding, which `DynamicImports` scopes already (`dynamic_imports.rs:16-17`; `crates/ecp-analyzer/tests/ts_js_import_bound_calls.rs`). |
| Ruby | `require` loads a file at run time and binds no name in a scope. `using` refinements are not modelled. |
| C | C has no import. `#include` inserts text, and preprocessor expansion is out of scope. |

## Today

### Parser phases

| Fact | Today |
|---|---|
| Phase order is per parser | Python collects in the match loop (`python/parser.rs:827`) and keeps tx sites as rows and columns (`:825`, `:979`). It binds them at `:1430`, then promotes `Class` to `Interface` (`:1432-1440`) and stamps owners (`:1482-1483`). Binding happens before finalizing ends. |
| Dedup after collect | Dart dedups nodes (`dart/parser.rs:483`) before it binds tx scopes (`:541`). The order is right by convention. |
| Bind after the last push | Go pushes nodes up to `go/parser.rs:949` and binds at `:1044`. |
| Innermost rule | `enclosing_fn_idx_by_span` (`framework_helpers.rs:208-221`) takes the smallest area. Go, Ruby, Dart, Python (`resolve_tx_scopes`, `python/parser.rs:1604`) and the fan-out source use it. |
| Concatenation | Vue joins the node lists of its `<script>` blocks (`vue/parser.rs:231`). It emits no indexed record, so nothing shifts today. |

### Raw index and the global mapping

| Fact | Produced | Read |
|---|---|---|
| A record with a raw index | `RawTxScope`: 24-bit node index plus `FrameworkId` (`ecp-core/src/analyzer/types.rs:945-982`, limit `:955`). Producers: `go/parser.rs:259`, `python/parser.rs:1604`, `ruby/parser.rs:289`, `dart/parser.rs:590`, `framework_helpers.rs:902`, `:930`, `:955`, `:984`, `:1010`. | `post_process/tx_scope_edges.rs:56-118` |
| Other raw-index users | Decorator routes take the decorated node (`resolution/builder.rs:1403`). Overrides take the candidate's own index (`post_process/overrides.rs:166`). Closure references use `innermost_enclosing` (`builder.rs:2166-2179`). | same |
| Prefix sum of node counts | Rebuilt in 13 places: `builder.rs:1398`, `:1712`, `:1880`, `:2160`, `:2219`, `:2325`, `:2902`; `tx_scope_edges.rs:66`, `:70`, `:118`; `overrides.rs:128`, `:182`, `:217`; `decorates_edges.rs:174`; `php/laravel_routes.rs:1653`. | each pass |
| Slot alignment | Pass 1 pushes a tombstone for a uid collision so that `nodes.len()` stays equal to the slot count (`builder.rs:1108-1118`, `:1198-1223`). An overload gets its own uid and slot (`:1123-1137`). The survivor is the first occurrence (`uid_seen`, `:1119`, `:1228`). | every prefix sum |
| Nodes after the raw range | Routes (`builder.rs:1409-1419`, `:1479-1489`), entry points (`:2072-2081`), processes (`:563`), detector nodes, then `File` nodes at the tail (`:746-788`). | — |
| Overlay slots | `base_len + nodes.len()` per fragment symbol (`ecp-core/src/session/view.rs:379`). Fragment symbols come from `graph.nodes` in order (`ecp-cli/src/session/overlay_writer.rs:177-179`), so symbol `i` is raw node `i`. | `OverlayView` |

### Endpoints by name

`SymbolTable::lookup_in_file` returns the first node with the name (`resolution/index.rs:847-852`; warning at `:840-846`). These producers find an endpoint with it:

| Record or edge | Field read | Lookup | Effect |
|---|---|---|---|
| `UsesPathLiteral` | `RawPathLiteral.enclosing_symbol`, `enclosing_owner` (`types.rs:600`, `:604`) | `post_process/path_literal_nodes.rs:77` | the first same-name node. A module-top literal gets no edge (`:74-76`; type doc `types.rs:584-585`). |
| `QueriesTable` | `RawSqlRef.enclosing_symbol` (`types.rs:647`) | `post_process/sql_table_edges.rs:57` | the first same-name node. Module-top SQL gets no edge (`:53-56`). |
| `Publishes`, `Subscribes` | `RawEventTopic.enclosing_fn`, empty when the detector has no capture (`types.rs:928-930`) | `post_process/event_topic_mirrors.rs:113` | the first same-name node, or no edge |
| `HasProperty` to `SchemaField` | `RawSchemaField.owner_class` (`types.rs:907`) | `post_process/schema_field_mirrors.rs:96` | the first same-name node |
| `Defines` | the raw node itself | `post_process/scope_defines.rs:45`, `:80` | two same-name nodes both resolve to the first |
| `HandlesRoute` (call form) | `RawRoute.handler: Option<String>` (`types.rs:507`) | `builder.rs:1508` | the first same-name node |
| entry point `References` | `ep.uid` (a handler name) | `builder.rs:2058` | the first same-name node |
| framework `References` | `RawFrameworkRef.source_name` (`types.rs:663`) | `builder.rs:2784` | `<module>` (`framework_helpers.rs:39`) never matches, so a module-level ref is dropped (`builder.rs:2785`) |
| fan-out `References` | `RawFanoutRef.source_name` | `enclosing_fn_idx_by_span` filtered by name, then a name lookup (`builder.rs:2818-2828`) | mostly raw index already |
| `Fetches` | none | first raw node of the file (`builder.rs:1864-1883`); the client calls come from a second disk read (`:1914-1916`) | the edge starts at an unrelated declaration |
| `CallMeta` | `RawCallMeta.caller_name`, `caller_span`, `call_index` (`types.rs:1045-1060`) | key `(caller_span, call_index)` (`builder.rs:117-125`, `:2971`) | pairs by span |
| `FunctionMeta` | `RawFunctionMeta.span` (`types.rs:376-378`) | span lookup (`builder.rs:2110-2160`) | pairs by span |

### Confidence today and the class each value maps to

| Edge | Today | Site | C4 class |
|---|---|---|---|
| `HandlesRoute`, decorator | 1.0 | `builder.rs:1425` | `Exact` (raw index) |
| `HandlesRoute`, call | 1.0, first of name | `builder.rs:1514` | `Exact` when unique; `Ambiguous` when 2 or more |
| `Fetches` | 0.8 exact path, 0.6 templated | `builder.rs:1965-1967` | `Exact`, `Heuristic`; source `File` |
| framework `References` | producer's value | `builder.rs:2807` | by band |
| fan-out `References` | `base / sqrt(N)`, floor 0.1 | `builder.rs:2834` | `Ambiguous { N }` for N ≥ 2; `Heuristic` for N = 1 |
| `UsesPathLiteral` | 1.0 | `path_literal_nodes.rs:86` | `Exact` once the source is a raw index |
| `QueriesTable` | 1.0; target is the first `Class` of that name in the repo (`sql_table_edges.rs:30-36`) | `sql_table_edges.rs:70` | `Exact` for a unique table; `Ambiguous` otherwise |
| `Publishes`, `Subscribes` | 1.0 | `event_topic_mirrors.rs:122` | `Exact` |
| `EventTopicMirror` | 0.85, first publisher | `event_topic_mirrors.rs:164` | `Heuristic` (heuristic by kind) |
| `HasProperty` to `SchemaField` | 1.0 | `schema_field_mirrors.rs:131` | `Exact` |
| `MirrorsField` | 0.9 | `schema_field_mirrors.rs:185` | `Heuristic` (heuristic by kind) |
| `OpensTxScope` | 1.0 | `tx_scope_edges.rs:112` | `Exact` |
| `Overrides` | 1.0; supertype by simple class name, same-name classes merged (`overrides.rs:91-105`) | `overrides.rs:176` | `Heuristic` until the supertype comes from the heritage facts |
| `Imports`, module step | 0.9 for every step | `imports_edges.rs:215`, `:391` | steps 3a, 3b (`:270-287`), 3e (`:311`): `Exact`. Steps 3c (`:288`), 3d (`:296`), 3f (`:358`): `Heuristic`, below 0.7, own reason |
| `Defines` | 1.0 | `scope_defines.rs:92`, `:117` | `Exact`, child by raw index |
| call tiers | 1.0 to 0.4 | `resolution/heuristics.rs:43-57` | area 1 decides (Open point 6) |

Values with no mapping:

| Value | Why |
|---|---|
| `enclosing_owner` (`types.rs:604`, `:649`) | The bound node carries its own `owner_class`. A PathLiteral uid hashes this field (`path_literal_nodes.rs:49-58`); see Stored data. |
| `enclosing_symbol`, `enclosing_fn`, `source_name`, `caller_name`, `owner_class` of a schema field | Replaced by `RawEndpoint`. |
| `RawExportAlias.local` (`types.rs:1069-1072`) | An export table that area 1's import tiers read by name. It is not an endpoint. |
| entry-point `ep.score` (`builder.rs:2091`) | A score, not a binding. Area 8 keeps it. |
| `Decorates` to a synthetic `Annotation`, 0.8 (`post_process/decorates_edges.rs:123-166`) | An unresolved target stored as a name node. Open point 8. |
| `RawDocumentBlock` (`types.rs:654`) | Not wired into the graph (`builder.rs:1269-1272`). |
| `BlindSpot` (`types.rs:1021`) | Metadata with its own path and span. |

### Overlay today

`OverlayView` masks base edges of a dirty file only for `Calls` and `ReadsField` (`ecp-core/src/session/view.rs:995`) and rebuilds `References` for five reasons (`:1002-1011`). It skips builder-made node kinds (`fragment_emits`, `:1297`). An L1 fragment holds symbols, imports and export aliases (`overlay_writer.rs:119-123`); the reader accepts format V5 only (`overlay_reader.rs:76`). The overlay replays no raw record of the table above except `Import`, and it rebuilds no edge kind that a `RecordKind` yields.

### Import scope today

| Fact | Today |
|---|---|
| `RawImport` | Four fields: `source`, `imported_name`, `alias`, `binding_kind`. No site, no scope (`ecp-core/src/analyzer/types.rs:659-672`). The shared constructor takes those four (`driver/imports.rs:6-18`). |
| Rows in effect file-wide | Python's `import_statement` pattern has no top-level anchor (`python/queries.scm:36-37`). Rust's nested-use walk visits every `use_declaration` in the tree (`rust/parser.rs:615-618`). Neither row records where it binds. |
| Forms not emitted | C++ captures `#include` and namespace aliases only (`cpp/queries.scm:252-264`). `using_declaration` is a name-reference skip kind (`cpp/parser.rs:550`), not an import. |
| Scope solved at parse time | `DynamicImports` (TypeScript, JavaScript, Python) holds a name from the end of its declaration to the end of its scope (`dynamic_imports.rs:16-17`) and rewrites the call. Its `RawImport` rows carry no scope. |
| Dedup | Go and C++ sort and dedupe imports on recipe fields only (`driver/imports.rs:30-49`; `go/parser.rs:519`, `cpp/parser.rs:574`). |
| Tier order | Tier 1 `same_file` returns before any import tier (`ecp-core/src/analyzer/name_tiers.rs:296-308`). The named-import tier runs at `:374`, and `global` runs last (`:515`). |
| Lexical input | The caller's span: `LexicalFunctions::call_resolution` (`ecp-core/src/analyzer/lexical.rs:694`), called with the caller symbol's span in the overlay (`ecp-core/src/session/view/resolution.rs:5819`). |
| Overlay memo | Keyed by `(file_ord, target)`, then by name (`ecp-core/src/session/view/resolution.rs:2842-2849`). |
| Stored | `ImportFact` has no scope (`ecp-core/src/graph.rs:855-867`); the builder copies the four fields (`resolution/builder.rs:1106-1112`). L1 fragments store `Vec<RawImport>` (`ecp-cli/src/session/overlay_writer.rs:148`). |
| Known wrong edge | `test_build_rust_use_inline_mod_item_bare_call_binds_it_not_top_level_namesake` is ignored: Tier 1 binds the top-level `g`, and the `use` of `mod user` records no `mod` (`crates/ecp-analyzer/tests/rust_macro_and_inline_mod_calls.rs:733-744`). |

## Consumers

### Area 1: one scope lookup

Reads: the scope table, the `scope` and `site` of each `RawImport`, and the lookup outcome rows of "Scope table and import bindings".

Must stop: Tier 1 binding before any import tier (`ecp-core/src/analyzer/name_tiers.rs:296-308`) for a language with a scope table; the caller's span as the lexical input (`ecp-core/src/analyzer/lexical.rs:694`); a named-import memo key without the occurrence (`ecp-core/src/session/view/resolution.rs:2842`).

### Area 2: owner split

Reads: `RawIdx` as the type of a lexical parent link (C1 `lexical_parents` returns file-local indexes); `RawIndexMap::node` and `slot`.

Must stop: relying on the tombstone push alone to keep slots aligned (`builder.rs:1108-1118`). When A-A gives twins distinct uids, pass 1 passes fewer `merged` pairs; the map does not change.

### Area 4: imports in the workspace scope

Reads: `Evidence`, `HEURISTIC_CEILING`, `Evidence::check`.

Must stop: emitting a guessed module match at 0.9 with the exact reason (`imports_edges.rs:288-309`, `:358-383`, `:391`).

### Area 5: one read path (the records the overlay replays)

Reads: `RawIndexMap::new(base_len, …)` for overlay slots, and `RecordKind::rel_types` for the stale-edge caveat. Until the four conditions of Open point 4 hold, the caveat names `Fetches` for a dirty set that holds a route file or a file with fetch calls.

Must stop:

- Counting overlay slots by hand (`view.rs:379`).
- Masking a base edge kind that the overlay does not rebuild (`view.rs:995`). Until the incremental overlay replays a kind, the caveat names that kind's `rel_types`.
- Replaying a record by name. A replayed record carries a `RawEndpoint`, so a fragment needs no global index.

### Area 7: post-parse edges carry endpoints and evidence

Reads: every type. Area 7 owns the producer side.

Writes: a `RawEndpoint` in place of each name field in the "Endpoints by name" table, bound by `RecordKind::bind_site` or `bind_name`; one `RawIndexMap` per build.

Must stop:

- `lookup_in_file` in `path_literal_nodes.rs:77`, `sql_table_edges.rs:57`, `event_topic_mirrors.rs:113`, `schema_field_mirrors.rs:96`, `scope_defines.rs:45`, `:80`, `builder.rs:1508`, `:2058`, `:2784`.
- The 13 prefix-sum copies listed in Today.
- Dropping a module-top record that has a `File` endpoint.
- `Overrides` at 1.0 from a simple class name (`overrides.rs:91-105`, `:176`).
- `Fetches` from the first node of the file (`builder.rs:1871-1883`).

### Area 8: one route and contract record

Reads: `RawEndpoint` for the handler, `bind_name`, `Evidence`.

Maps to C1 `HandlerRef` (`ecp-core/src/contracts/identity.rs:1312-1327`; C1 Open point 13, `C1-identity.md:212`):

| C4 | C1 |
|---|---|
| `Node(i)` whose node is a closure: a `Function` whose name starts with `<anonymous:` (`is_anonymous_closure`, `builder.rs:2183-2185`) | `Closure { uid }` of `RawIndexMap::node`; prints `inline_anonymous` |
| any other `Node(i)` | `Declaration { uid }` of `RawIndexMap::node` |
| `Unresolved(Named(s))`, after every cross-file pass | `Unresolved { name: s }` |
| `Unresolved(Anonymous)`, `Unresolved(Inline(_))` | `Inline { line }`, line = site start row + 1; prints `inline_anonymous` |
| no endpoint | `Absent` |

Must stop: `RawRoute.handler` as `Option<String>` (`types.rs:507`); binding the first of two same-name handlers (`builder.rs:1508`).

### Area 10: shared parse driver

Reads: `ParsePhase`, `RecordKind::endpoint_rule`, `bind_enclosing`, `bind_name`.

Must stop:

- Binding before the node list is final (`python/parser.rs:1430` before `:1432-1483`).

A driver extraction keeps raw records identical, as plan §10 requires. A change of a binding rule is a capability change with an approved difference.

### Cross-area conflict rows that name C4

| Pair | Requirement | Status |
|---|---|---|
| 2, 7 | Raw slots kept in pass 1 (`builder.rs:1198`). Area 7 waits for the index mapping, not for A-A. | Satisfied. `RawIndexMap` reads node counts only. The tombstone-to-survivor pairs are an optional input that A-A can shrink. |
| 4, 7 | `Imports` guesses at 0.9 (`imports_edges.rs:391`). | Satisfied by `Evidence::Heuristic` and `HEURISTIC_CEILING`. |
| 7, 8 | `Fetches` source (`builder.rs:1871`) and `RawRoute.handler` as a string (`types.rs:507`). | Satisfied. `Fetches` takes `RawEndpoint::File`; the handler takes `RawEndpoint` with `Unbound::Named` for a cross-file name. |
| 7, 10 and 8, 10 | Raw indexes (`types.rs:936`, the `RawTxScope` doc; the type is at `:945`) and route producers rewritten by a later driver. | Satisfied by `ParsePhase`, the per-kind `EndpointRule` and the concatenation rule. |
| 5, 7 and 5, 8 | The overlay masks only some edge kinds (`view.rs:995`) and skips builder nodes (`:1297`). | Partly. `RecordKind::rel_types` names the stale kinds for the caveat. Replay itself is the incremental overlay's work. |
| 1, 10 | Area 10's producers emit the scope facts that area 1's lookup reads. | Open until the scope table slice. "Scope table and import bindings" fixes the records and the outcome first. A producer change is a capability change with an approved difference. |

## Stored data

| Identifier | Change |
|---|---|
| `BUILDER_FINGERPRINT` (`ecp-core/src/registry/commit_meta.rs:93`, now `schema25`) | Bump it in each slice that changes what the build outputs for the same source. That covers a `LocalGraph` field (area 7, area 8's handler), a binding rule (area 10), an endpoint, a confidence or an evidence class. An `Imports` or `Overrides` confidence change counts, although it changes no `LocalGraph` field. The parse cache dir is keyed by it (`ecp-cli/src/parse_cache.rs:3-7`, `:31-36`), so old entries are never read. A stored graph with another fingerprint is rebuilt (`ecp-cli/src/auto_ensure.rs:346`, `meta.json` fallback `:355`). |
| `GRAPH_FORMAT_VERSION` (`ecp-core/src/graph.rs:36`, now 15) | Only for a change of the archived `graph.bin` layout. None in this contract: `LocalGraph` lives in the parse cache, not in `graph.bin` (`parse_cache.rs:96-99`), and evidence is stored as confidence (Open point 1, option a). Condition 3 of Open point 4 is a layout change and bumps it. |
| L1 fragment encoding (`FRAGMENT_FORMAT_V5`, `ecp-core/src/session/overlay.rs:57`) | None in this contract. The incremental overlay adds a format that stores records with `RawEndpoint`. |
| Dirty manifest | None. |
| View cache key (`ecp-cli/src/session/view_cache.rs:35-48`) | Follows `BUILDER_FINGERPRINT` (`:40`) and the fragment bytes (`overlay_reader.rs:90-91`), so a fingerprint bump also invalidates it. Nothing extra. |

Uid effect: a PathLiteral uid hashes the record's `enclosing_owner` (`path_literal_nodes.rs:49-58`). When area 7 takes the owner from the bound node, the uid changes where the two spellings differ (plan §2: `Walk<'s, 't>` against `Walk`). That difference needs a baseline entry with owner area 7.

Proof per the repo `CLAUDE.md`: index with a parse cache that the previous binary wrote. Assert that no old entry is read, and that a path literal in a method of a trait impl starts at that method, not at the bodiless trait declaration.

## Open points

1. **Where the class is stored.** (a) No column: a stored edge reads back as `Exact` or `Heuristic` by band and `RelType::is_heuristic`. (b) A sparse edge-evidence table, as `call_metas`; a `GRAPH_FORMAT_VERSION` bump. (c) A token in `reason`; consumers parse reasons today (`builder.rs:2086` entry-point reason). Recommendation: (a). A post-parse pass emits no edge for `Ambiguous`; it records a `BlindSpot` with the count. Fan-out refs keep one edge per candidate below 0.7, because they enumerate candidates on purpose.
2. **A record inside a tombstoned twin.** (a) Redirect to the survivor, as typed. (b) Drop the record. Recommendation: (a). Every name lookup already lands on the survivor, and an edge must not start at a node with an empty name. A-A removes most twins.
3. **The `File` node index.** `File` nodes are appended after the path-literal and SQL passes (`builder.rs:723-737`, `:746-788`). (a) Append `File` nodes before the post passes; node order in `graph.bin` changes, the format does not. (b) Keep `GlobalEndpoint::File(FileOrd)` and emit those edges after the append. Recommendation: (b).
4. **`FetchSite` has no record.** `fetch_urls::extract` reads text and returns no position (`fetch_shape/fetch_urls.rs:62`), and pass 1.6 reads the file from disk at build time (`builder.rs:1914-1916`). That read bypasses the parse cache and area 3's snapshot. (a) `File` endpoint from the current pass. (b) A parse-time record with a site, bound to the enclosing callable. Recommendation: (a) in the first area 7 or 8 slice, (b) in area 10.

   Until then the phase-2 overlay cannot replay URL and method facts. `view.rs:995` does not mask `Fetches`, so a base `Fetches` edge stays as the base commit matched it, also after a route rename. Caveat: when the dirty set holds a file that adds, removes or changes a `Route`, or a file with fetch calls, the answer names `Fetches` as stale (matched at the base commit). The caveat goes away only when all four conditions hold:

   | # | Condition | Owner |
   |---|---|---|
   | 1 | `LocalGraph` carries a parse-time `FetchSite` record: method, URL text, templated flag, site, and a `RawEndpoint` from `bind_enclosing` with the `File` fallback. A `BUILDER_FINGERPRINT` bump. | area 10 |
   | 2 | An L1 fragment format after V5 stores those records (`overlay.rs:57`, reader `overlay_reader.rs:76`). | area 5 |
   | 3 | `graph.bin` keeps the fetch sites of clean files, or the route table they match against. A `GRAPH_FORMAT_VERSION` bump. | area 8 |
   | 4 | The overlay masks the base `Fetches` edges of a dirty file, and re-matches every dirty route against all fetch sites and every dirty fetch site against all routes. | area 5 |
5. **Inline handler.** Closed by C1 Open point 13: option (a). Some parsers emit `<anonymous:…>` closure nodes (`builder.rs:2183-2185`), and only when the body holds a call. A closure node is a `Node` endpoint and maps to `HandlerRef::Closure { uid }`. A handler with no node stays `Unbound::Inline` at its argument's span and maps to `Inline { line }`. Both print `inline_anonymous`, so `ecp routes` keeps its values, and the uid lets `impact` reach the calls inside the closure. See the Area 8 mapping table.
6. **Call tiers and B's outcome type.** Decided. R1a (`393f52ae`) carries the true count: `NoEdge::Ambiguous(u32)` (`ecp-core/src/analyzer/outcome.rs:92`), so `Evidence::Ambiguous { candidates }` can take it. Before R1a, the variant carried no count. The `Global` tier (`Tier::Global`, `outcome.rs:29`) is Heuristic class: a name-only match with no import or scope evidence. Its confidence stays 0.7 (`Tier::confidence`, `outcome.rs:43`). That value sits at `HEURISTIC_CEILING` (`raw_records.rs:471`), so a `Global` edge stays untagged. R-A4 may revisit the class or the value. C4 does not reclassify other call tiers.
7. **Accepted kinds of a path literal or SQL literal.** The type doc names Function, Method and Const (`types.rs:595`). A literal in a module-level `Variable` binds `File`. Recommendation: keep. The accepted kinds are one shared rule in `RecordKind`, so widening them changes every language. A change ships with tests for the 14 mainstream languages (repo `CLAUDE.md`), each with a literal in the newly accepted kind and one outside it.
8. **`Decorates` to a synthetic node at 0.8** (`decorates_edges.rs:123-166`). (a) Keep: the synthetic node is the honest unresolved marker (`SYNTHETIC_FILE_IDX`). (b) Drop it below 0.7. Recommendation: (a); area 7 confirms.
9. **`RawTxScope` packing.** Its 24-bit index (`types.rs:955`) is narrower than `RawIdx`. `RawTxScope::new` checks the limit with a `debug_assert` only (`types.rs:962-970`), so in a release build index `2^24` packs as `0` and the record binds the file's first node. Recommendation: keep the packing; read it as `RawIdx(node_idx())`. A producer packs only through `RawIdx::to_packed24`, which checks in every build. On `None` it drops the record and records a `BlindSpot` with the file and the site; it never packs a truncated index. Area 7 moves the check into a checked `RawTxScope` constructor that returns `Option`.
10. **An import binding with several targets** (a Rust glob pair that places one name twice, two Python star imports). (a) `NoEdge::Ambiguous(n)` with its `Ladder` reach: the constructor fallback still runs. (b) `OpaqueBinding`: the count is lost. (c) A reason with the `Call` reach that carries the count. Recommendation: (c). It keeps the fallbacks from binding a hidden namesake, and the count feeds `Evidence::Ambiguous`.
11. **`DynamicImports` and the scope table.** (a) Keep the parse-time rewrite (`dynamic_imports.rs:30`). (b) Emit its names as import bindings of their occurrence. Recommendation: (a) in the first slice; (b) when TypeScript adopts the table, with `crates/ecp-analyzer/tests/ts_js_import_bound_calls.rs` as its guard.
