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

Declarations, imports and local names share one scope model. A parser emits a scope table per file. Each import binding and each local binding names its destination occurrence, and each call and name use records its use site. The `Imports` edge keeps its `FileOnly` endpoint: the edge source and the binding scope are two facts. The existing type `ImportScope` (`ecp-core/src/session/import_scope.rs:167`) holds the overlay's module-discovery facts, and it is not this scope.

| Term | Meaning |
|---|---|
| scope occurrence | One source region where names bind: the file, a module, a namespace block, a type body, a callable body, or an anonymous block. |
| scope table | The scope occurrences of one file, in pre-order. |
| binding | A name that an occurrence binds: a declaration, an import binding or a local binding. |
| import binding | One `RawImport` row, read with its destination, its site and its form. |
| local binding | A name that an occurrence binds with no graph node: a parameter or a local variable. |
| directive | A Python `global` or `nonlocal` statement. It binds no value. It redirects the bindings and the reads of its name in its occurrence to a target occurrence. |
| destination | The occurrence where a binding takes effect. It is the innermost occurrence that contains the site, unless a directive moves it (Python `global`, `nonlocal`). |
| target recipe | The fields that say what an import binding names: `source`, `imported_name`, `alias`, `binding_kind`, as today. |
| synthetic row | A `RawImport` that `DynamicImports` makes for a literal dynamic load. Its alias starts with `@import`. |
| use site | The start position of one call or one name use, and the innermost occurrence that contains it. |
| environment | The bindings in reach at a use site, in source order. |
| in reach | Visible at a use site under the language rule. |
| opaque binding | A binding in reach whose target resolves to no declaration in the graph: a local binding, an import of a missing module or an external package, or an unknown target. |

| Type (planned, `ecp-core/src/contracts/raw_records.rs`) | Kind | Purpose |
|---|---|---|
| `ScopeId` | stored `u32` | The position of an occurrence in its file's scope table. `ScopeId(0)` is the file. C1 "Scope occurrence identity" fixes its stability. |
| `ScopeKind` | stored enum | `File`, `Module`, `Namespace`, `Type`, `Callable`, `Block`. |
| `RawScopeOccurrence` | stored record | `parent: Option<ScopeId>`, `kind`, `extent: Span`, `decl: Option<RawIdx>` (the declaring node; C1 derives its anchor), `flags: u8` (`INLINE` and `ANONYMOUS`, set on a C++ `Namespace` occurrence only). The name keeps it apart from C1's `Scope`, which is part of a declaration key. |
| `BindingForm` | stored enum | `Named`, `Wildcard`, `Module`, `Synthetic`. The name keeps it apart from today's `BindingKind`, which classifies C and ES bindings. |
| `RawLocalBinding` | stored record | `scope: ScopeId` (the destination), `name`, `site: Span`, `form`: `Param` or `Local`. |
| `RawDirective` | stored record | `scope: ScopeId` (the occurrence that holds it), `name`, `site: Span`, `kind`: `Global` or `Nonlocal`. |
| `UseSite` | stored record | `scope: ScopeId`, `at: (u32, u32)`: the 0-based row and column where the use starts. |
| `ScopeError` | result | An occurrence, a binding or a use site that breaks an invariant below. |

| Record | New field | Meaning |
|---|---|---|
| `LocalGraph` | `scopes: Vec<RawScopeOccurrence>` | The file's scope table. |
| `LocalGraph` | `local_bindings: Vec<RawLocalBinding>` | The file's local bindings, in source order. |
| `LocalGraph` | `directives: Vec<RawDirective>` | The file's directives, in source order. |
| `RawImport` | `scope: ScopeId` | The destination of the binding. |
| `RawImport` | `site: Span` | The span of the import clause. |
| `RawImport` | `form: BindingForm` | The form, as the producer reads it from the syntax. |
| `RawNode` | `call_sites: Vec<UseSite>` | Entry `i` is the use site of `calls[i]` (`ecp-core/src/analyzer/types.rs:523`). |
| `RawNode` | `read_sites: Vec<UseSite>` | Entry `i` is the use site of `field_reads[i]` (`types.rs:530`). |

A producer that removes or reorders an entry of `calls` or `field_reads` does the same to its site. So `RawCallMeta.call_index` (`types.rs:1415-1416`) keeps its meaning. A language with no scope table leaves both site vectors empty. A query never reads the AST to recover a site.

A producer may drop a local binding when no use in the subtree of its destination reads that name. This keeps the parse cache small: locals are not nodes (`python/queries.scm:62-65`).

**Recipe adapter.** The producer writes the form, because the recipe alone cannot always tell two forms apart: C# `using N;` and `using static T;` have one encoding (`c_sharp/queries.scm:110-128`). A language-aware adapter then reads the recipe and the form, and it gives the local name and the member. It shares the rules of `import_binding` (`ecp-core/src/analyzer/import_binding.rs:138-148`, `:156-206`); `reexport::named_import` is a lookup, not this adapter.

| Recipe today | Form | Local name |
|---|---|---|
| `imported_name` `*`, no alias: a Python star import (`python/queries.scm:29-32`), a Rust glob (`rust/parser.rs:787`) | `Wildcard` | none |
| `imported_name` `*`, alias `a`, in a language outside `fqn_language`: a namespace import (`import_binding.rs:115-132`) | `Module` | `a` |
| `imported_name` `*`, alias `None`, `*` or `static:*`, in Java, Kotlin or PHP (`import_binding.rs:170-183`) | `Wildcard` | none |
| `imported_name` `*`, another alias `a`, in PHP: a group member `use A\{B as C}` (`import_binding.rs:184-191`) | `Named`; the member is the last segment of `source` | `a` |
| alias that starts with `@import` (`dynamic_imports.rs:89-93`) | `Synthetic` | none |
| any other `imported_name` `n` | `Named` | the alias, else `n` |

A form that this table does not list gets its row in the slice that converts its language, before the walk reads it.

| Item | Invariant |
|---|---|
| Root | `scopes[0]` is the only `File` occurrence. It has no parent, and its extent is the whole file. |
| Parent | Every other occurrence has a parent with a lower `ScopeId`. So the parent chain is acyclic and ends at the root. |
| Nesting | A parent's extent contains the extent of each child. Two sibling extents do not overlap. |
| One region | A callable body or a type body is one occurrence. The producer does not also emit a `Block` with the same extent. |
| `decl` | `Some(i)`: raw node `i` passes `is_scope_container` (C1), and its kind fits `kind` (next table). `File` and `Block` take `None`. A container with no node takes `None`, such as a TS `namespace` today (C1 Open point 14). |
| Binding site | The `site` of an import binding or a local binding lies inside the extent of its destination. |
| Destination | With no directive, the destination is the innermost occurrence that contains the site. A directive for the name in that occurrence moves the destination to the directive's target: the root for `Global`, the nearest enclosing `Callable` occurrence that binds the name for `Nonlocal`. |
| Directive | A directive is never a candidate. It takes no part in the order rule, the merge rule or the opaque count. A read of its name in its occurrence continues the walk at its target occurrence. A `Nonlocal` with no target is a malformed file: the read gives `OpaqueBinding`. |
| Use site | `at` lies inside the extent of `scope`, and no child of `scope` contains `at`. |
| Declaration scope | A declaration belongs to the innermost occurrence that contains its span, its own body excluded. It needs no stored field. |
| Synthetic row | Its destination is the root, and its site is the span of the first load it stands for. The walk never matches it by a source name. The parser already scoped each load by range (`dynamic_imports.rs:16-17`) and wrote the rewritten callee (`:44-59`). |
| Binding order | An import list with a scope table stays in source order. Dedup drops a row only when its recipe, form, destination and site all equal those of another row: a double capture. `Imports` edges keep their own dedup by target per file (`post_process/imports_edges.rs:55-56`). |
| Phases | The producer fixes the table order before it writes a `ScopeId`. `decl` binds in `BindRecords`, as a `RawIdx` does. |

| `ScopeKind` | `decl` kinds |
|---|---|
| `Module` | `Module` |
| `Namespace` | `Namespace` |
| `Type` | `Class`, `Struct`, `Enum`, `Interface`, `Trait`, `Impl` |
| `Callable` | `Function`, `Method`, `Constructor` |
| `File`, `Block` | none |

**Concatenation.** Code that joins the raw graphs of several embedded sections (Vue `<script>` blocks) follows these rules. The embedded parse already shifts node spans into file coordinates (`offset_span`, `sfc_common.rs:93`, called at `:185`, with the offsets that `vue/parser.rs:221-222` and `:231-232` pass), and the join is at `vue/parser.rs:236-237`.

| Rule | Statement |
|---|---|
| Coordinates | Every extent, site and `at` is in file coordinates before the join. |
| Root | The joined table gets one new root: `File`, with the whole file as extent. |
| Section | The root of each input becomes a `Block` child of the new root, with the extent of its section. Sections do not overlap, so Nesting holds. |
| Remap | Each `ScopeId` of input `k` (a `parent`, an import `scope`, a local binding or directive `scope`, a use-site `scope`) gains `1 +` the total table length of inputs `0 .. k`, so it points into its own section. A synthetic row is the exception: its `scope` becomes the new root, `ScopeId(0)`. Each `decl` gains the node offset of input `k`, as each `RawIdx` does. |
| Visibility | Two sections are siblings, so neither sees the other through the walk. A language rule that merges sections names them; no language with a scope table has one in rounds 1 and 2. |

**Scope lookup.** Area 1 owns the visibility policy. C4 fixes its input, its consumers and its result.

| Rule | Statement |
|---|---|
| Input | The file, the `UseSite`, the name and the target kind. The caller's span is not the input, because two sibling blocks of one callable differ only by use site. |
| Walk | The walk starts at the use site's occurrence. It ends at the root, or after the first boundary occurrence that the language rule names. At each occurrence, the candidates are the bindings with that destination and the deferred bindings that join there ("C++ namespaces"). |
| Language rule | A per-language rule decides four things: the order rule (a binding visible before its site, or only after it), the merge rule (which candidates of one occurrence stand), the occurrences the walk skips or links, and the boundary occurrences (next table). |
| Paths | A path-qualified name does not take the walk. Rust `self::`, `super::` and `crate::` resolve through the module tree, as the qualified-head consumer does. |
| Stop | At each occurrence, the walk first drops a wildcard candidate whose module resolves to an indexed file that does not export the name. A wildcard whose module does not resolve stays: it may bind the name. The first occurrence that keeps a standing candidate ends the walk, and an outer occurrence never overrides it. When none remains, the walk continues outward. The order rule of the language decides which bindings stand; for Python it is the Python row of the order table. |
| Lexical verdict | For a language with a scope table, the walk produces the lexical verdict that `ladder::call` reads (`cx.lexical`, `ecp-core/src/analyzer/ladder.rs:2108-2124`) and the `lexical` input of `ladder::name_ref` (`ladder.rs:432-439`). `LexicalFunctions` does not produce them for that language. So no declaration-only verdict returns before the walk compares imports. |
| Replaces | For that language, the walk also replaces Tier 1 (`same_file`), the named-import tier and the wildcard pass for a bare name. A walk result that rests on a `Wildcard` binding alone waits until the heritage tier binds nothing, as today's wildcard pass does (`ladder.rs:788-790`). |
| Backing list | The environment is a view over the file's full import list in its stored order. A position, such as `NamedImport.position` (`ecp-core/src/analyzer/reexport.rs:1026`), indexes that full list, never a filtered copy. |
| Memo | The memo caches the use-site-independent part: for one file, one occurrence, one name and one target kind, the ordered bindings of the name (declarations, import bindings, local bindings) with their sites and resolved targets. The key is (`FileOrd`, `ScopeId`, name, target kind); a `ScopeId` alone is file-local. The pick at the use site applies the order rule to that list and is never cached, so a declaration between two uses changes the answer (`from a import g; g(); def g(): ...; g()`). The memo lives no longer than the view generation; the overlay clears its named-import memo when the facts change (`ecp-core/src/session/view/resolution.rs:329-332`). |
| Cost | The occurrence index and the per-occurrence name index are built at parse or refresh time. The pick costs one scan of the bindings of one name in the walked occurrences. A query never walks the AST, never scans the whole import list, never expands a wildcard to every name, and never rebuilds the environment of a clean file. |

| Language | Order rule | Occurrence rule |
|---|---|---|
| Rust | hoisted: a `use` is visible in its whole occurrence | each occurrence is separate. A `Module` occurrence is a boundary: a bare name does not see the items or the `use` of an outer module (`crates/ecp-analyzer/tests/rust_import_scope.rs:329-336`) |
| Python | (a) In the `Callable` occurrence that holds the use site, a name that the occurrence binds anywhere is local there. The last binding of the name before the site stands. When none comes before the site, the name is not yet bound, and the result is `OpaqueBinding`. (b) In the root or a `Type` occurrence that holds the use site, the last binding before the site stands; when none comes before it, the walk continues outward. (c) In an enclosing occurrence, reached from a nested callable, the last binding of the name in the whole occurrence stands. The textual position of the use site does not limit it, because the read runs when the callable runs: `from b import g`, `def caller(): g()`, `from c import g` binds `c.g`. A directive redirects the read (`crates/ecp-analyzer/tests/python_import_scope.rs:451-464`) | the walk skips a `Type` occurrence when the use site is in a callable nested in it |
| C# | hoisted inside its namespace block | each namespace block is separate |
| PHP | statement order | each namespace section is separate |
| C++ | from the point of declaration | the blocks of one namespace are linked; see "C++ namespaces" below |
| TypeScript | hoisted for `import`; the round-2 slice confirms `import X = A.B` | each `namespace` and `declare module` body is separate |

**C++ namespaces.**

| Rule | Statement |
|---|---|
| Namespace path | Two blocks are one namespace when their namespace paths are equal. The namespace path is the chain of `Namespace` occurrences from the root, each segment the name of its `decl` node. It is not the C1 anchor: an unscoped C1 uid hashes the file path (C1 "Declaration identity"), so `outer` in `b.h` and `outer` in `a.cpp` have two anchors but one path. C1 declaration identity does not change. |
| Anonymous namespace | Its segment is `<anonymous:path>`, with the path of its own file. So two files never share one anonymous namespace, also when both names are empty. |
| Linked blocks | The blocks of one namespace path are linked. A later block sees the using-declarations and using-directives that an earlier block holds, in the order of the blocks. |
| using-declaration | A candidate in its own occurrence. |
| Directive reach | A namespace-scope using-directive is visible after its point in its block and in every later linked block of the same namespace (`crates/ecp-analyzer/tests/cpp_import_scope.rs:621`). A block-scope using-directive is visible after its point in its block and in the blocks inside it only. |
| Join | The names of a using-directive join the candidates at the join namespace: the nearest namespace that encloses both the directive and the nominated namespace ([namespace.udir]), compared by namespace path. The walk adds them at the first walked occurrence whose namespace path is the join namespace (the root for the global namespace), after every inner level. |
| Unknown join | When the nominated namespace does not resolve, the join namespace is unknown. A binding at a level inside the directive's innermost enclosing namespace stands as usual, because no join level lies there. From that namespace outward, the directive stands as a candidate with no target, so any result it could change becomes `OpaqueBinding`. |
| Inline and anonymous | An inline or an anonymous namespace acts as a namespace-scope using-directive that its enclosing namespace holds at the namespace's start, nominating itself. The producer sets the flag on the occurrence, and the lookup index derives the implicit directive at refresh time; no row is stored for it. A qualified name through the enclosing namespace (`o::g`) also reaches the members of an inline namespace. |

Example: `namespace outer { namespace b { g } }`, `::g`, then `namespace outer { caller() { using namespace b; g(); } }` binds `outer::b::g` at the second `outer` block, before the walk reaches `::g`. The same holds when the first `outer` block is in `b.h`.

The C++ rules are pinned by `test_cpp_import_scope_reopened_scope_namespace_declaration_visible_in_reopening`, `test_cpp_import_scope_reopened_scope_namespace_directive_visible_in_reopening` and `test_cpp_import_scope_directive_hoists_to_common_namespace_nested_namespace_binds_outer_decl` (`crates/ecp-analyzer/tests/cpp_import_scope.rs`, from `:124`). The C# and PHP rows are pinned by their `reopened_scope` tests.

**Import consumers.** For a language with a scope table, every path that reads imports reads the environment of the use site. None reads the whole file's list.

| Consumer | Site today | Reads |
|---|---|---|
| named-import tier | `name_tiers.rs:374` | replaced by the walk |
| import-member tier (Python, Java, Kotlin, PHP) | `name_tiers.rs:339` | the environment |
| qualified head (`m.run`, `A::new`) | `name_tiers.rs:384` | the environment: the head binds through bindings in reach only |
| wildcard pass | `ladder::wildcard_member`, `ladder.rs:790` | replaced by the walk, ordered after heritage |
| import-only path after `Shadowed` | `ladder.rs:2116-2122`, `import_only` at `:2288` | not reached: the walk never returns `Shadowed` |
| constructor fallback | `ladder::constructed_type`, `ladder.rs:2415` | the environment, when its `resolve` lets the import tiers bind |
| Python `FromReceiver` submodule member | `ladder.rs:2127-2135` | the environment |

Named exceptions, which read the file's full list:

| Exception | Why |
|---|---|
| `Imports` edges | A dependency belongs to the file. |
| re-export following of a module | Only rows whose destination is the root or a `Module` occurrence export. A row in a callable or a block never re-exports. |
| synthetic rows | The parser scoped them by range; the rewritten callee names the alias. |
| Python parse-time receiver typing | It keeps its own guard (`python/receiver_types.rs:90-99`) in rounds 1 and 2. A disagreement with the environment is a defect that the Python matrix shows. |

All names in this table are under `ecp-core/src/analyzer/` except the Python receiver file.

**Lookup outcome.** The walk classifies its result in three steps. (1) The walk finds the stopping occurrence by the Walk and Stop rules, with directives and deferred bindings applied. (2) In that occurrence, the order rule and the merge rule give the standing candidates, and each resolves to its targets. A wildcard whose module does not resolve gives no target. (3) The result counts distinct target declarations. For Python, the Python row of the order table picks the standing binding, so a later star import that stands and exports the name overrides an earlier binding.

| Walk result | `Resolution` | Later ladder tiers | Kotlin retry | Constructor fallback | Outer chain level |
|---|---|---|---|---|---|
| one distinct target | `Bound(hit)` | as §3.2 row `Bound` | as that row | as that row | as that row |
| two or more distinct targets | `NoEdge(AmbiguousBinding(n))`, `reach()` is `Call` | skip | skip | skip | `NoEdge(ChainBroken)` |
| standing candidates, no target: a local binding, a binding of the use site's own callable that is not yet bound at the site (Python rule (a)), a missing or external module | `NoEdge(OpaqueBinding)`, `reach()` is `Call` | skip | skip | skip | `NoEdge(ChainBroken)` |
| no binding of the name in reach; the name can be bound elsewhere in the file (a sibling occurrence, a declaration out of reach) | `Unknown` | run, except the tiers that "Replaces" lists | runs | runs | `Unknown`, then the level fallback |
| no binding of the name in the file | `Unknown` | the same | runs | runs | the same |

The walk never returns `Shadowed`: it has already compared every binding of the file. `Shadowed` keeps its meaning for a language with no scope table. Several candidates that name one declaration give `Bound`.

The reach of both new reasons is `Call`, because a later tier or a fallback binds a namesake that the binding hides: `global` runs last in `name_tiers` (`ecp-core/src/analyzer/name_tiers.rs:515`). Their evidence classes are `Unresolved` and `Ambiguous { candidates: n }`: no edge. `NoEdge::External` keeps its `Ladder` reach (`ecp-core/src/analyzer/outcome.rs:106-111`) for the import-member tiers that use it today.

The walk adds the two reasons to `NoEdge` (`outcome.rs:87-103`) and two rows to the fallback table of shared-resolution §3.2. The rows live here: §3.2 records the design state at `ff75103a`, and this contract is the current rule.

**Stored surfaces.**

| Surface | Must carry | Identifier |
|---|---|---|
| parse cache (`LocalGraph`) | `scopes`, `local_bindings`, `directives`; `scope`, `site` and `form` on each `RawImport` (`types.rs:659-672`); `call_sites` and `read_sites` on each `RawNode` (`types.rs:510-544`). `LocalGraph.imports` is at `types.rs:1456`. | bump `BUILDER_FINGERPRINT` (`ecp-core/src/registry/commit_meta.rs:190`, now `schema52`) |
| L1 fragment | `FragmentFile.imports` is a `Vec<RawImport>` (`ecp-cli/src/session/overlay_writer.rs:148`), so the binding fields travel with it. `FragmentFile` gains `scopes`, `local_bindings` and `directives`. `FragmentSymbol` gains `call_sites` and `read_sites` beside `calls` and `variant_refs` (`overlay_writer.rs:100`, `:102`), and the overlay replays them. | a new fragment format value. The reader accepts one value (`ecp-cli/src/session/overlay_reader.rs:206`). V7 shipped in v0.17.0, so the new fields do not extend it in place. |
| `graph.bin` | `ImportFact` (`ecp-core/src/graph.rs:855-867`) gains `scope`, `site` and `form`. `ZeroCopyGraph` gains every file's scope table, in CSR form as `file_imports` (`graph.rs:1319-1320`). The overlay reads a clean module's imports from these facts (`graph.rs:850-852`). `graph.bin` stores no use site. A clean file of the invalidation closure gets fresh use sites only when its re-parse succeeds and its fragment is accepted (`ecp-core/src/session/view.rs:121-125`; `ecp-cli/src/session/closure_store.rs:264-268`, `:458-463`, `:680-684`). Every other clean caller keeps its indexed edges. A re-parse is omitted only when the parse returns `content_hash == [0; 8]` with a blind spot (`is_omitted`, `closure_store.rs:701-703`); an omitted re-parse, or a stored fragment that is stale or invalid, marks the closure `not_computed` and adds the kinds that may be stale (`closure_store.rs:705-716`; `StaleClosure`, `ecp-core/src/session/closure.rs:148-166`). A parse with no mtime, or whose fragment fails to archive, is dropped with no entry and no flag (`closure_store.rs:684-687`): its file keeps its indexed edges, and the caveat does not mark it. A file that the closure does not name gets no flag either. Its `names` list holds only the removed names that a clean ambiguous call may now bind to (`closure.rs:159-161`). | bump `GRAPH_FORMAT_VERSION` (`graph.rs:47`, now 20) |
| overlay memo and view cache | The memo key of "Scope lookup". Today the key is `(file_ord, target)` (`ecp-core/src/session/view/resolution.rs:2842`). | The view cache key already hashes `VIEW_CACHE_FORMAT`, `BUILDER_FINGERPRINT`, the binary, the `graph.bin` stamp and the accepted fragment inputs (`ecp-cli/src/session/view_cache.rs:39-51`), so the bumps above invalidate it. Bump `VIEW_CACHE_FORMAT` only when the encoding of a cached view changes. |

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
| Reach | file, namespace or module, callable body, anonymous block; a Rust outer `use` that an inline `mod` does not see; a C++ using-directive that joins at the common namespace, also when that namespace is reopened; the same with the first `outer` block in `b.h` and the caller in `a.cpp` (namespace path, not anchor); a C++ inline namespace and an anonymous namespace whose `g` a caller of the enclosing namespace binds; two anonymous namespaces in two files that do not merge |
| Isolation | sibling occurrences that bind `g` to two targets; no reach outside the occurrence; a namespace reopened in one file, with the language's occurrence rule |
| Precedence | an inner import against an outer declaration; an inner declaration against an outer import; a local or a parameter of the same name |
| Order | a use before and after the binding; a hoisted item against a statement-order binding; in Python, a use before a later import of the name in one function (`OpaqueBinding`, no outer edge), and `from a import g`, `from b import g`, `from a import g` in one scope; `from a import g; g(); def g(): ...; g()` in one function, queried in both orders; `from b import g`, `def caller(): g()`, `from c import g` binds `c.g`; a C++ namespace-scope using-directive that a later block of the namespace sees, and a block-scope one that a sibling block does not see; a C++ directive whose nominated namespace does not resolve gives `OpaqueBinding` for `g` in the enclosing namespace, and an inner local `g` still binds |
| Directive | Python `global g` then `from m import g` in a function; `global g; g()` under a module-level `from b import g` binds `b.g`; `nonlocal g` in a nested function |
| Consumers | a qualified head `m.run()` with `m` bound to two modules in sibling occurrences; the import-member tier, the constructor fallback and Python `FromReceiver` under the same split |
| Dynamic load | in Python, two sibling functions that load one module: one synthetic row, and both calls bind |
| Identity | inserted whitespace; an inserted unrelated occurrence; swapped sibling subtrees of different sizes |
| Failure | a missing module, an external target, an ambiguous target, unsupported or malformed syntax: no fallback edge; an inner wildcard whose resolved module does not export the name, under an outer binding that does |
| Cache | one occurrence, one name, one target kind, two environments; query A then B, and B then A, give one result |
| Overlay | a changed import source, alias or occurrence; a deleted or moved import; a changed dependency export with the caller's bytes unchanged |

**Coverage today.** The six test files assert the full-build target and the overlay parity. They cover Reach, Isolation, Precedence, Order, Directive, Consumers (qualified head), Dynamic load and Failure. A case that ecp gets wrong today is `#[ignore = "needs ..."]`, and its overlay-parity test runs. The raw-record assertions and the Identity, Cache and Overlay-edit groups need the fields above, so they land with the slice that adds those fields.

Contract tests in `raw_records.rs`: an acyclic parent chain, each binding and use site inside its occurrence, the directive rule, the concatenation remap, a round trip through the parse cache, the L1 fragment and `graph.bin` that keeps every occurrence, a dedup that keeps rows with different sites, and a lookup result that does not depend on query order.

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

`OverlayView` masks a base edge that starts in a re-parsed file only for a kind that it rebuilds (mask ⊆ rebuild, `ecp-core/src/session/view.rs:1245-1275`): `Calls`, `ReadsField`, `HasMethod`, `Decorates`, `Overrides`, `Extends` and `Implements` at the file's declarations; `Defines` and `Imports` at its declarations and its `File` node; `HasProperty` and the record edges when the records replayed. It rebuilds `References` for five reasons only: closure, overload set, enum-variant use, enum-variant pattern and function value (`view.rs:1290-1300`). A fragment never carries the node kinds `File`, `Import`, `Route`, `Process`, `Document`, `EntryPoint` and the four record kinds, so a re-parse alone never suppresses such a base node (`fragment_emits`, `view.rs:1965-2007`). The record replay is the exception for its four node kinds: it suppresses a base `PathLiteral`, `TransactionScope`, `SchemaField` or `EventTopic` node that no replayed node pairs with (`ecp-core/src/session/incremental.rs:136-152`). A clean file of the invalidation closure is parsed again and resolved as a dirty one when its re-parse succeeds and its fragment is accepted (`view.rs:121-125`, `OverlayView::build_with_configs` at `:1167-1194`). A closure file whose re-parse is omitted (`content_hash == [0; 8]` with a blind spot, `is_omitted` at `ecp-cli/src/session/closure_store.rs:701-703`), or whose stored fragment is stale or invalid, keeps its indexed edges, and the closure is marked not computed (`closure_store.rs:264-268`, `:458-463`, `:680-684`, `:705-716`). A parse with no mtime, or whose fragment fails to archive, is dropped with no entry and no flag (`closure_store.rs:684-687`), so its file keeps its indexed edges with no mark. An L1 fragment (`FragmentFile`) holds symbols, imports, export aliases, the file's raw records and its language after parse (`ecp-cli/src/session/overlay_writer.rs:146-159`). The raw records are `OverlayFileRecords`: path literals, SQL references, transaction scopes, schema fields and event topics, each endpoint bound (`overlay_writer.rs:150`; `ecp-core/src/session/view.rs:442-448`; format note at `ecp-core/src/session/overlay.rs:70-78`). The reader accepts format V7 only (`ecp-cli/src/session/overlay_reader.rs:206`). Given those records, the overlay replays `Import` and the five record kinds, and it rebuilds their nodes and their `UsesPathLiteral`, `QueriesTable`, `OpensTxScope`, `HasProperty`, `Publishes` and `Subscribes` edges (`view.rs:88-93`). It replays no `Route`, `FrameworkRef`, `FanoutRef`, `CallMeta` or `FetchSite` record.

### Import scope today

| Fact | Today |
|---|---|
| `RawImport` | Four fields: `source`, `imported_name`, `alias`, `binding_kind`. No site, no scope (`ecp-core/src/analyzer/types.rs:659-672`). The shared constructor takes those four (`driver/imports.rs:6-18`). |
| Rows in effect file-wide | Python's `import_statement` pattern has no top-level anchor (`python/queries.scm:36-37`). Rust's nested-use walk visits every `use_declaration` in the tree (`rust/parser.rs:615-618`). Neither row records where it binds. |
| Forms not emitted | C++ captures `#include` and namespace aliases only (`cpp/queries.scm:252-264`). `using_declaration` is a name-reference skip kind (`cpp/parser.rs:550`), not an import. |
| Scope solved at parse time | `DynamicImports` (TypeScript, JavaScript, Python) holds a name from the end of its declaration to the end of its scope (`dynamic_imports.rs:16-17`) and rewrites the call (`dynamic_imports.rs:44-59`). It makes one synthetic row per distinct specifier (`:80-99`), so two sibling functions that load one module share one row. Its rows carry no scope. |
| Use sites | `RawNode.calls` holds callee text only (`ecp-core/src/analyzer/types.rs:523`); the producer pushes the callee and no position (`calls.rs:200-201`). `RawCallMeta` keys a call by its caller's span and `call_index` (`types.rs:1412-1416`). L1 `FragmentSymbol.calls` is text too (`ecp-cli/src/session/overlay_writer.rs:100`). |
| Early lexical verdict | `ladder::call` returns a `Bound` lexical verdict before any import tier (`ecp-core/src/analyzer/ladder.rs:2108-2115`). |
| Dedup | Go and C++ sort and dedupe imports on recipe fields only (`driver/imports.rs:30-49`; `go/parser.rs:519`, `cpp/parser.rs:574`). |
| Tier order | Tier 1 `same_file` returns before any import tier (`ecp-core/src/analyzer/name_tiers.rs:296-308`). The named-import tier runs at `:374`, and `global` runs last (`:515`). |
| Lexical input | The caller's span: `LexicalFunctions::call_resolution` (`ecp-core/src/analyzer/lexical.rs:694`), called with the caller symbol's span in the overlay (`ecp-core/src/session/view/resolution.rs:5819`). |
| Overlay memo | Keyed by `(file_ord, target)`, then by name (`ecp-core/src/session/view/resolution.rs:2842-2849`). |
| Stored | `ImportFact` has no scope (`ecp-core/src/graph.rs:855-867`); the builder copies the four fields (`resolution/builder.rs:1106-1112`). L1 fragments store `Vec<RawImport>` (`ecp-cli/src/session/overlay_writer.rs:148`). |
| Known wrong edge | `test_build_rust_use_inline_mod_item_bare_call_binds_it_not_top_level_namesake` is ignored: Tier 1 binds the top-level `g`, and the `use` of `mod user` records no `mod` (`crates/ecp-analyzer/tests/rust_macro_and_inline_mod_calls.rs:733-744`). |

## Consumers

### Area 1: one scope lookup

Reads: the scope table, the local bindings, the `scope`, `site` and `form` of each `RawImport`, the use sites, and the consumer and outcome tables of "Scope table and import bindings".

Must stop: Tier 1 binding before any import tier (`ecp-core/src/analyzer/name_tiers.rs:296-308`) for a language with a scope table; a `Bound` lexical verdict from `LexicalFunctions` for that language (`ecp-core/src/analyzer/ladder.rs:2108-2115`); the caller's span as the lexical input (`ecp-core/src/analyzer/lexical.rs:694`); an import consumer that reads the whole file's list outside the named exceptions; a named-import memo answer that depends on the use site but is keyed without it (`ecp-core/src/session/view/resolution.rs:2842`).

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
6. **Call tiers and B's outcome type.** Decided. R1a (`393f52ae`) carries the true count: `NoEdge::Ambiguous(u32)` (`ecp-core/src/analyzer/outcome.rs:92`), so `Evidence::Ambiguous { candidates }` can take it. Before R1a, the variant carried no count. The `Global` tier (`Tier::Global`, `outcome.rs:29`) is Heuristic class: a name-only match with no import or scope evidence. Its confidence stays 0.7 (`Tier::confidence`, `outcome.rs:48`). That value sits at `HEURISTIC_CEILING` (`raw_records.rs:487`), so a `Global` edge stays untagged. R-A4 may revisit the class or the value. C4 does not reclassify other call tiers.
7. **Accepted kinds of a path literal or SQL literal.** The type doc names Function, Method and Const (`types.rs:595`). A literal in a module-level `Variable` binds `File`. Recommendation: keep. The accepted kinds are one shared rule in `RecordKind`, so widening them changes every language. A change ships with tests for the 14 mainstream languages (repo `CLAUDE.md`), each with a literal in the newly accepted kind and one outside it.
8. **`Decorates` to a synthetic node at 0.8** (`decorates_edges.rs:123-166`). (a) Keep: the synthetic node is the honest unresolved marker (`SYNTHETIC_FILE_IDX`). (b) Drop it below 0.7. Recommendation: (a); area 7 confirms.
9. **`RawTxScope` packing.** Its 24-bit index (`types.rs:955`) is narrower than `RawIdx`. `RawTxScope::new` checks the limit with a `debug_assert` only (`types.rs:962-970`), so in a release build index `2^24` packs as `0` and the record binds the file's first node. Recommendation: keep the packing; read it as `RawIdx(node_idx())`. A producer packs only through `RawIdx::to_packed24`, which checks in every build. On `None` it drops the record and records a `BlindSpot` with the file and the site; it never packs a truncated index. Area 7 moves the check into a checked `RawTxScope` constructor that returns `Option`.
10. **An import binding with several targets.** Decided before implementation: the walk first applies the language's order and merge rules, then counts distinct targets ("Lookup outcome"). Two or more give `NoEdge::AmbiguousBinding(n)` with the `Call` reach. So a Python star import that a later one overrides is no ambiguity, and two imports of one declaration give one edge. The `Call` reach keeps the fallbacks from binding a hidden namesake, and `n` feeds `Evidence::Ambiguous`.
11. **`DynamicImports` and the scope table.** Decided for round 1, Python included: the parse-time rewrite stays (`dynamic_imports.rs:44-59`). Its synthetic rows are dependency rows with destination root ("Synthetic row" invariant), and the walk never matches them by a source name. A later slice may emit each loaded name as a local binding and drop the rewrite; `crates/ecp-analyzer/tests/ts_js_import_bound_calls.rs` and the Python dynamic-load case then guard it.
