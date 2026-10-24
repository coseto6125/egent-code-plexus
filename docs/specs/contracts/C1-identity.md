# C1: identity

Status: contract, phase 1. Types: `crates/ecp-core/src/contracts/identity.rs`. No code calls them yet.

Plan row (`docs/specs/2026-10-25-architecture-direction.md`, "Contracts before code"): declaration identity, lexical parent, the target request syntax and a typed ambiguity result; route identity and handler identity as separate keys. Consumers: areas 1, 2, 5, 8, 9.

Every `file:line` below was read on branch `feat/arch-p1-contracts` at `1d267693`. Commit `01cf2641` (C2 only) does not move them.

## Defines

### Terms

| Term | Meaning |
|---|---|
| base uid | The hash of a declaration key before overload numbering: `DeclKey::uid`. |
| stored uid | The uid a node carries in `graph.bin` and in the overlay: `UidSlot::uid`. |
| anchor | The semantic identity of a declaration: the base uid plus its discriminator. Pairing, `diff` and child uids read it. |
| scoped | A declaration whose lexical parent is a callable. It has no owner. |
| qualified path | The owner segments of a declaration, then the names of its lexical parents, outermost first. |

### Types

| Type | Kind | Purpose |
|---|---|---|
| `DeclKey` | value | The identity inputs of one declaration: kind, path, owner, name, scope, discriminator. |
| `Scope` | enum | `Unscoped`, or `Lexical { parent_anchor }`. |
| `Discriminator` | enum | `ImplTrait`, `Signature` or `Tag`: a semantic fact that separates equal keys. |
| `FileDecl` | input row | One declaration of a file, as `assign_file` reads it. |
| `UidSlot` | result | `First(uid)`, `Later { first, ordinal, uid }` or `Duplicate { first, uid }`. |
| `Uniqueness` | result | `Unique`, or `Shared { position, count }`. |
| `DeclIdentity` | result | The slot, the anchor and the uniqueness of one declaration. |
| `IdentityError` | result | The invariant that an input breaks. |
| `TargetRequest` | value | The parsed target text plus the `--kind` and `--file` filters. |
| `Qualifier`, `Segment` | enum | The part of a target before the name. |
| `TargetSyntaxError` | result | Why a target text does not parse. |
| `CandidateFacts`, `PathSegment` | input row | What the resolver knows of one declaration with the requested name. |
| `FileMatch` | enum | The tier of a `--file` match: `Substring`, `Suffix`, `Equal`. |
| `TargetResolution` | result | `Unique(Candidate)`, `Ambiguous(Ambiguity)` or `NotFound { named }`. |
| `Candidate`, `Ambiguity` | result | One matching declaration; the full list when several match. |
| `RouteKey` | value | The identity of one endpoint: file, verb, composed path. |
| `HandlerRef` | enum | What serves a route: `Declaration { uid }`, `Inline { line }`, `Unresolved { name }`, `Absent`. |
| `RouteBinding` | value | One `HandlesRoute` fact: a route key and a handler. |

Functions: `canonical_path`, `canonical_owner`, `scoped_uid`, `assign_file`, `pair_by_anchor`, `is_scope_container`, `lexical_parents`, `file_match`, `normalize_file_needle`, `select`, `resolve_target`, `join_route_path`.

### Declaration identity: fields and invariants

| Field | Invariant |
|---|---|
| `DeclKey.path` | Repo-relative and `/`-separated, with no leading `./`. `canonical_path` gives that form. A constructor rejects any other spelling. |
| `DeclKey.owner` | The type, namespace or module that declares the node (meanings M1, M2 and M4 of design A §1.2). `Some("")` is `None`, as `uid::compute` already treats it. The spelling has no generic argument list: `canonical_owner("Walk<'s, 't>")` is `Walk`. The separators `.` and `::` stay as the producer writes them. |
| `DeclKey.name` | The bare declared name, as `Node.name` stores it. |
| `DeclKey.scope` | `Unscoped`: `uid()` equals `uid::compute(kind, path, owner, name)`, today's value. `Lexical { parent_anchor }`: `owner` is `None` (I1), the kind does not keep overloads (I3), and `uid()` is `scoped_uid(parent_anchor, kind, name)`. |
| `DeclKey.discriminator` | Optional. Its text is not empty. It enters the anchor and never the uid, so adding one changes no stored uid. |
| `scoped_uid` stream | Starts with byte `0x01` and is at least 14 bytes long. A `compute` stream starts with an ASCII letter. An `overload` stream is 12 bytes. No two streams are equal. |
| anchor stream | Equals the base uid without a discriminator. With one, it starts with `0x02` and is at least 13 bytes long. |

### File assignment: `assign_file(path, decls)`

| Rule | Statement |
|---|---|
| Input | One `FileDecl` per declaration of one file, in source order. `parent` comes from `lexical_parents`. A parent can come before or after its children. |
| Anchors first | A child's key hashes its parent's anchor, so the function resolves each parent chain before the child. |
| Errors | A parent out of range or a cycle gives `BadParent`. A scoped row with an owner gives `ScopedWithOwner`. A scoped row of a kind that keeps overloads gives `ScopedOverloadKind`. |
| `UidSlot` | Groups by base uid in source order. The first keeps the base uid. A later one of a kind that keeps overloads, or a later scoped one, gets `uid::overload(base, ordinal)` with a 1-based ordinal: `Later`. A later one of another kind is `Duplicate` and keeps the base uid, as the builder does today. |
| `Uniqueness` | Groups by anchor. One member: `Unique`. Several: `Shared { position, count }` for each. |
| Positional rule | A `Shared` position proves nothing about which declaration an edit kept. It never pairs a node, and it never forms a target alone. |

Example: `impl Display for A { fn fmt { fn pad } }` then `impl Debug for A { fn fmt { fn pad } }`. Both `fmt` rows have base uid `U`, so the second gets `overload(U, 1)`. Their anchors differ, because `ImplTrait("Display")` and `ImplTrait("Debug")` differ. Each `pad` hashes its own `fmt` anchor. Swap the two blocks, and each `pad` keeps its uid. The test `test_assign_file_swapped_impls_keep_child_uids` asserts it.

Example: one function holds two blocks, and each block declares `h`. Both `h` rows have one parent anchor, so they share an anchor: `Shared`. The second gets `Later`, not `Duplicate`, so both stay nodes. The test `test_assign_file_sibling_blocks_shared_not_duplicate` asserts it.

### Overlay pairing: `pair_by_anchor(base, dirty)`

A dirty declaration replaces a base declaration of the same file only when their anchor occurs exactly once on each side. Every other dirty declaration is unpaired. An unpaired base declaration is suppressed in the dirty view, as a deleted one is.

### Lexical parent: `lexical_parents(decls, scoped_kinds)`

| Rule | Statement |
|---|---|
| Input | `(kind, span)` per declaration of one file, plus the language's opt-in kind list (design A §5.2). The list is a parameter, so this contract does not depend on C5's language value. |
| Containers | `Function`, `Method`, `Constructor`, `Class`, `Struct`, `Enum`, `Interface`, `Trait`, `Impl`, `Namespace`, `Module` (`is_scope_container`). |
| Link | The innermost container that strictly contains the node, by `innermost_enclosing` (`ecp-core/src/analyzer/types.rs:40`). A non-container ancestor between them is skipped. |
| Scoped | The kind is in the opt-in list, the kind does not keep overloads, and that container is a callable. A class or module in between breaks the link. |
| Output | `Some(index)` of the callable in the same slice, or `None`. An empty opt-in list gives `None` for every node. |
| One derivation | The parser, the index build, the overlay view, the overlay reader and incremental mode call this function. No consumer builds a lexical scope string. |

### Target request syntax

```
target    := [ root ] path [ "@" line ]
root      := "::" | "."                      (only before a bare name)
path      := segment ( sep segment )*        (the last segment is the name)
sep       := "." | "::"                      (outside a generic argument list)
segment   := name | "<" type " as " trait ">"
line      := 1-based decimal start line
```

| Request | Matches |
|---|---|
| `load` | every declaration named `load`: any owner, scoped or not. |
| `::load` or `.load` | a `load` with no owner and no lexical parent. |
| `Engine.load`, `Engine::load` | a `load` whose qualified path ends with the segment `Engine`. |
| `a::b.f`, `a.b.f`, `a::b::f` | one request: segments `a`, `b`, name `f`. |
| `<A as Display>.fmt.pad` | the `pad` inside the `fmt` of `impl Display for A`. The type and the trait compare by their last segment. |
| `pad@42` | a `pad` that starts on line 42. |
| `--kind method` | `NodeKind::from_str` of the text; an unknown kind is a syntax error. |
| `--file cmd/main.go` | `\` becomes `/`, a leading `./` goes. A resolver keeps the best tier present: `Equal`, then `Suffix` (`/needle`), then `Substring`. |

| Rule | Statement |
|---|---|
| Qualifier match | The request segments equal a segment-bounded suffix of the candidate's qualified path. This holds for every language. |
| Canonical qualifier | A qualifier segment goes through `canonical_owner`, so `Walk<'s>.next` matches owner `Walk`. The final name keeps its text, so `operator<` stays a name. |
| `@` | Only `@` plus digits at the end is a line. `route@fx` is a name. `@0` is an error. |
| Render | `TargetRequest::render` joins segments with `.`, prints a root as `::name` and a line as `@N`. `parse(render(r))` gives back `r`'s qualifier, name and line. |

### Typed target result: `resolve_target(request, named)`

| Rule | Statement |
|---|---|
| Input | Every declaration with the requested name, from the merged graph's name index. The caller has already removed deleted declarations and synthetic nodes. |
| One match | `Unique(Candidate)`. |
| Several | `Ambiguous(Ambiguity)`. `total()` is the true count. The command never picks the first. A command that writes files refuses it. |
| None | `NotFound { named }`. `named` counts the declarations with that name before the filters: 0 means the name is not declared. |
| Reusable target | Each candidate of an ambiguity carries `reusable`: a request that selects that candidate alone among all of `named`. It escalates from the qualified name, then adds `--file`, then `@line`, then `--kind`. `None` means no target text and no filter can separate it, for example two macro-generated nodes on one line. |
| Old error | `Ambiguity::to_error` builds today's `EcpError::AmbiguousSymbol` (`ecp-core/src/error.rs:22-31`) with the same fields, so the message does not change when a command adopts the type. |

### Route identity and handler identity

| Field | Invariant |
|---|---|
| `RouteKey.file` | The registering file, canonical as `DeclKey.path`. |
| `RouteKey.verb` | Upper-case ASCII, not empty, no whitespace. `GRPC`, `MOUNT` and `ANY` are verbs too. |
| `RouteKey.path` | The full path with every prefix composed. `join_route_path` composes with one `/` at the joint and a leading `/`; it keeps a trailing `/`. |
| `RouteKey::name` | `"<VERB> <path>"`, today's stored `Route` name. `split_name` inverts it on the first space. |
| `RouteKey::uid` | `uid::compute(Route, file, None, name)`, today's value. Both route producers call this one function. Two producers that see one endpoint make one key, so they make one node. |
| Separation | A route key never holds the handler. A handler key never holds the route. |
| `HandlerRef::Declaration { uid }` | The stored uid of the handler declaration. A route can have several bindings, and a handler can serve several routes. |
| `HandlerRef::Inline { line }` | An anonymous function at the registration. No node exists. |
| `HandlerRef::Unresolved { name }` | The registration names a handler that binds no declaration. It is never printed as inline. |
| `HandlerRef::Absent` | The producer saw no handler. |

## Today

### Declaration identity

| Fact | Produced at | Read at | Maps to |
|---|---|---|---|
| uid inputs | `uid::compute` (`ecp-core/src/uid.rs:17-27`) | builder Pass 1 (`ecp-analyzer/src/resolution/builder.rs:1092-1097`); overlay view (`ecp-core/src/session/view.rs:372-377`); incremental `node_uid_for` (`ecp-cli/src/incremental.rs:283-297`); builder skip sets (`builder.rs:3009`, `:3046`, `:3107`, `:3141`); `class_membership.rs:69` | `DeclKey::unscoped(...).uid()`, equal value |
| overload ordinal | builder (`builder.rs:1119-1127`, `SymbolTable::register_overload` at `resolution/index.rs:793`); overlay (`view.rs:378-385`) | `is_later_overload` recomputes `compute` and compares (`ecp-core/src/graph.rs:1483-1500`) | `UidSlot::Later`, same ordinal rule |
| same identity, other kind | builder pushes a tombstone (`builder.rs:1100-1118` comment, D1 collision recovery) | the tombstone has an empty name | `UidSlot::Duplicate`, unchanged; see Open point 4 |
| "keeps overloads" | `uid::keeps_overloads` (`uid.rs:33-35`): `Method`, `Constructor` | builder, view | used as is |
| overlay pairing | `dirty_base_by_uid` (`view.rs:348-355`), lookup at `view.rs:388`: by stored uid, so by position for overloads | `ViewNode.replaced_base` (`view.rs:254`) | `pair_by_anchor`; a `Shared` anchor pairs nothing |
| owner (M1, M2, M4) | parsers; `stamp_owner_class_by_span` (`ecp-analyzer/src/framework_helpers.rs:260`); Rust `enclosing_impl_type` (`rust/receiver_types.rs:286`); Rust `Impl` owner is the trait (`rust/parser.rs:402-460` comment) | `Node.owner_class` (`graph.rs:698-702`) | `DeclKey.owner` after `canonical_owner` |
| owner spelling | path literal owner `Walk<'s, 't>` beside function owner `Walk` (review page, postparse F1) | `ecp-analyzer/src/post_process/path_literal_nodes.rs` (producer not traced) | `canonical_owner`: `Walk` |
| Rust trait of an impl member | Method owner is the self type only (`enclosing_impl_type`); the `Impl` node's owner holds the trait | none for a member | `Discriminator::ImplTrait`, read from the enclosing `Impl` node |
| overload signature | none: `RawNode` has no parameter list (`ecp-core/src/analyzer/types.rs:411-444`) | — | `Discriminator::Signature` has no producer yet |
| YAML resource kind | not in the uid (review page: `Service` and `Deployment` share one identity) | — | `Discriminator::Tag` has no producer yet |
| diff identity | `SymbolKey { path, owner_class, name, kind }` (`ecp-cli/src/commands/diff/symbols.rs:48-57`), first wins (`:271-278`) | `ecp diff`, `ecp review` | the anchor plus `Uniqueness` |
| other identity keys | `impact --baseline` keys `(&'static str, String, String)`, the third field the name (`ecp-cli/src/commands/impact/baseline.rs:20`); it pairs anonymous closures by body hash (`:408-444`). peers key `(file, name)` (`ecp-core/src/peer/concern.rs:57-61`) | their own modules | not mapped; see Open point 9 |

### Lexical parent (meaning M3 of `owner_class`)

| Fact | Produced at | Read at | Maps to |
|---|---|---|---|
| Rust nested fn owner string | `enclosing_function_path` (`rust/receiver_types.rs:195`), called at `rust/parser.rs:351` (closure) and `:435` (Function) | `Node.owner_class` | `lexical_parents` link; owner `None` |
| Rust caller scope string | `rust_lexical_scopes` (`ecp-core/src/analyzer/same_file_pick.rs:545`), stored by `note_rust_lexical_scopes` (`resolution/index.rs:736-748`, called at `builder.rs:1266`) and `raw_owner_overrides` (`index.rs:196-203`) | Rust bare-call lookup | `lexical_parents` link; maps deleted |
| JS/TS nested fn owner | `stamp_js_function_owners` (`framework_helpers.rs:736`) | uid, display | `lexical_parents` link |
| Python/JS/TS owner on every unowned kind | `stamp_owner_fn_by_span` (`framework_helpers.rs:421`), called at `framework_helpers.rs:737` and `python/parser.rs:1483` | uid, display | Functions: `lexical_parents` link. Non-callables keep the string (design A §5.2) |
| Go `:=` Variable owner | `go/parser.rs` (owner set at `:625`) | uid | unchanged in this program (design A slice F) |
| lexical tier input | `LexicalFunctions::new` takes `(kind, name, has_owner, span)` (`ecp-core/src/analyzer/lexical.rs:19-54`); builder `:2281`, overlay `view.rs:559` | both call ladders | `(kind, span)` into `lexical_parents`, plus a parent link per function |
| Rust global candidate filter | `rust_free_global_candidate` in `lookup_global` (`resolution/index.rs:1059-1081`) and `nearest_global_copy` (`:1209`) | global tier | "scoped nodes are not global candidates" (design A I5), read from the parent link |

### Target request and ambiguity

| Fact | Produced at | Read at | Maps to |
|---|---|---|---|
| FQN split | `split_fqn_target`, last `.` (`ecp-cli/src/commands/symbol_id.rs:50-55`) | `impact` (`commands/impact/symbol.rs:133`), `resolve_candidates` (`symbol_id.rs:91`), `inspect` (`commands/inspect.rs:687`), `pattern` (`commands/pattern.rs:283`), `find-schema-bindings` (`commands/find_schema_bindings.rs:193`) | `TargetRequest::parse`. `a::b::f` now parses; today it is one bare name |
| owner match | `owner_matches`: equality, plus a last-segment match for `.rs` files only (`symbol_id.rs:57-64`); `inspect` uses equality (`inspect.rs:632`); `pattern` uses equality and a bare name means "no owner" (`pattern.rs:274-295`) | — | `Qualifier::matches`: segment-bounded suffix, every language |
| bare name in `rename` | top-level only: `owner_class.len == 0` (`ecp-cli/src/commands/rename.rs:332-345`); an owner form needs exact equality (`:314-331`) | rename stage 2 scans each file for the name alone (`rename.rs:410`) | bare name = every declaration; today's rename meaning is `::name` |
| bare name in `find` | no owner form at all (review page, cli F1: `ecp find Engine.load` returns `found:false`) | — | `Qualifier::Path` |
| file needle | `normalize_file_needle`, substring, then `narrow_to_path_suffix`: equality, then `/`-bounded suffix (`symbol_id.rs:193-221`) | `impact`, `path` | `normalize_file_needle`, `file_match`, best tier in `select` |
| later overload as target | skipped (`symbol_id.rs:113`) | — | a candidate like any other; an ambiguity separates it |
| ambiguity, `impact` | `matches.len() > 1` gives `EcpError::AmbiguousSymbol` (`commands/impact/symbol.rs:153-192`) | `peers plan` (`commands/peers_plan.rs:64`), telemetry (`telemetry_cli.rs:52`), batch (`impact/mod.rs:378`) | `Ambiguity::to_error`, equal fields |
| ambiguity, `inspect` | returns every match as a full block (`inspect.rs:806`); an `Impl` beside a primary type is dropped first (`inspect.rs:703-740`) | agent | `Ambiguous`; the `Impl` rule becomes a kind filter the command applies before `resolve_target` |
| ambiguity, `rename` | none: every match is renamed (review page, secondary RN1) | — | `Ambiguous`, refused |
| not found | `InvalidArgument("No symbol named …")` (`impact/symbol.rs:143-148`); `inspect` `status: error` (`inspect.rs:695-701`) | agent | `NotFound { named }` |

### Route and handler identity

| Fact | Produced at | Read at | Maps to |
|---|---|---|---|
| Route uid, decorator producer | `uid::compute(Route, path, None, "VERB path")` (`builder.rs:1407-1411`) | `ecp routes` (`commands/routes.rs`) | `RouteKey::uid`, equal value |
| Route uid, call producer | same expression (`builder.rs:1477-1481`) | same | `RouteKey::uid`, equal value |
| duplicate Route nodes | NestJS gives two `Route` nodes with one uid (review page, frameworks F2; producer pair not traced) | `ecp routes` prints two rows | one key gives one node |
| route name split | `split_route_name` (`commands/routes.rs:201`); `diff/routes.rs:77` | routes, diff | `RouteKey::split_name` |
| verb case | `route_detector.rs:225` upper-cases | — | `RouteKey::new` upper-cases |
| handler, decorator | the decorated node (`builder.rs:1403`, edge at `:1421-1427`) | `routes.rs` incoming `HandlesRoute` | `HandlerRef::Declaration` |
| handler, call | `RawRoute.handler: Option<String>` (`ecp-core/src/analyzer/types.rs:507`), bound by `lookup_in_file`, first node of that name (`builder.rs:1508`) | — | `HandlerRef::Declaration` through C4's endpoint; `Unresolved` when nothing binds |
| handler kinds printed | `named` (`routes.rs:478`), `unresolved_action` (`:424`), `inline_anonymous` for every route with no edge (`:446`) | agent | `Declaration` → `named`; `Unresolved` → `unresolved`; `Inline` → `inline_anonymous`; `Absent` → `absent` |
| EntryPoint uid | `compute(EntryPoint, path, None, "tag:handler_name")` (`builder.rs:2067-2073`); controller actions add the owner (`builder.rs:1737-1746`) | entry-point readers | not mapped; see Open point 8 |

## Consumers

| Area | Reads | Must stop |
|---|---|---|
| 1 (resolution) | `pair_by_anchor` for `replaced_base`; `lexical_parents` as the lexical tier input on both sides; "scoped means not a global candidate" for the global filter | pairing by stored uid (`view.rs:388`); the Rust lexical scope strings in `index.rs:196-203` and `same_file_pick.rs:545`; `has_owner` as the lexical admission input |
| 2 (owner split) | `DeclKey`, `Scope`, `Discriminator`, `assign_file`, `lexical_parents`, `canonical_owner` | writing M3 strings into `owner_class`; tombstoning a repeated scoped node; hashing a parent's positional uid; printing an owner the target syntax cannot read back |
| 5 (read path) | `TargetRequest::parse`, `resolve_target`, `TargetResolution`, `Ambiguity::to_error` | seven resolvers; the Rust-only suffix match; `rename` renaming every match; `inspect` returning full blocks for an ambiguity; a bare `rename` meaning top-level only |
| 8 (routes) | `RouteKey`, `join_route_path`, `HandlerRef`, `RouteBinding` | two `uid::compute(Route, …)` sites; one node per producer for one endpoint; a handler bound by first-name lookup; `inline_anonymous` for a named handler that did not bind |
| 9 (Cypher) | node identity is the merged node index: `DISTINCT` on a node keys on it (`ecp-core/src/cypher/executor.rs:462-468`, `value.rs:53-57`); the `uid` property is `UidSlot::uid` (`executor.rs:2816`) | deduplicating nodes by name; reading `ownerClass` (`executor.rs:2817`) as an identity |

### Cross-area conflicts that name C1

| Pair | Requirement | How C1 meets it |
|---|---|---|
| 1, 2 | the global candidate filter (`resolution/index.rs:1073`, now `:1059-1081`), the lexical input, the uid pairing (`view.rs:388`) | one `lexical_parents` gives both the filter fact and the lexical input; `pair_by_anchor` replaces uid pairing. The order of the slices stays as the plan's Order section states. |
| 2, 5 | the target resolver reads the owner (`symbol_id.rs:141`); `rename` treats an empty owner as top level (`rename.rs:332-345`) | the resolver reads the qualified path (owner plus parents), never the raw owner. The top-level meaning has its own syntax, `::name`. `rename`'s scoped exclusion becomes an ambiguity that `rename` refuses (Open point 5). |
| 2, 8 | Route uids made in two places (`builder.rs:1411`, `:1481`) | `RouteKey::uid` is the one constructor. It hashes no handler and goes through no `assign_file` numbering, so a uid change of area 2 does not move a Route uid. |

## Stored data

| Change | `GRAPH_FORMAT_VERSION` (15, `graph.rs:36`) | `BUILDER_FINGERPRINT` (`schema21`, `registry/commit_meta.rs:79`) | L1 fragment (`FRAGMENT_FORMAT_V5`, `session/overlay.rs:57`) | dirty manifest | view cache key (`VIEW_CACHE_FORMAT` 3, `view.rs:1060`) |
|---|---|---|---|---|---|
| adopt `DeclKey` for unscoped nodes, no discriminator | none | none | none | none | none |
| `assign_file` in the four paths that skip ordinals (A-A) | none | bump | none | none | follows |
| the parent link table (A-B) | bump | bump | none | none | follows |
| a language opts in to scoped uids (A-C, A-D, A-E) | none | bump | none: `FragmentSymbol` has kind, span and owner (`ecp-cli/src/session/overlay_writer.rs:85-96`) | none | follows |
| repeated scoped node becomes `Later`, not a tombstone | none | bump | none | none | follows |
| `canonical_owner` applied by a producer | none | bump | none | none | follows |
| `Discriminator::ImplTrait` | none | none: the anchor is not stored | none: the `Impl` nodes are in the fragment | none | follows |
| `Discriminator::Signature` or `Tag` | none | bump (new parser fact) | bump: `FragmentSymbol` lacks the fact | none | follows |
| `pair_by_anchor` in the overlay | none | none | none | none | follows: the binary path, size and mtime are in the key (`ecp-cli/src/session/view_cache.rs:36-43`) |
| `RouteKey` with composed prefixes; one node per key | none | bump | none | none | follows |
| `TargetRequest` / `resolve_target` | none | none | none | none | none |

## Open points

| # | Decision | Options | Recommendation |
|---|---|---|---|
| 1 | Does the discriminator enter the stored uid? | (a) anchor only; (b) the uid too | (a). The uid of every Rust trait-impl method stays as today. Only pairing, `diff`, targets and child uids need the trait. (b) changes uids of nodes that have no defect. |
| 2 | Who supplies `Discriminator::Signature`? | (a) a parser field per language; (b) none, and `Shared` stays | (b) for now. Java, Kotlin, C#, C++, Swift and Dart overloads stay `Shared`, so they pair nothing and need `@line` as a target. Add (a) only when a measured case needs it. |
| 3 | What pairs a `Shared` group in the overlay? | (a) nothing; (b) equal `content_hash` first, then the one remaining pair | (a) in the first slice; the view suppresses the base members and the caveat names them. (b) needs a per-symbol hash in `FragmentSymbol` (L1 bump). `impact --baseline` already pairs closures by body hash (`impact/baseline.rs:408-444`), so (b) has a precedent. |
| 4 | A `Duplicate` of a kind that does not keep overloads (a `#[cfg]` twin) | (a) tombstone, as today; (b) `Later` for every kind | (b), after `is_later_overload` (`graph.rs:1483`) stops recomputing `compute` and reads the slot. The twin stays findable; the review counts 15 hidden `Function` tombstones in this repo. Owner: area 2. |
| 5 | Does a bare name match scoped nodes? | (a) yes, and a tie is an ambiguity; (b) no, as design A §5.5 proposes for `rename` | (a). One rule for every command. `rename` refuses the ambiguity and lists `::helper` and the scoped target. This departs from design A §5.5. |
| 6 | The scope of a route key | (a) the registering file, as today; (b) the service root | (a). It keeps today's uids. Two services in one monorepo stay apart when their route files differ. |
| 7 | Where does `ImplTrait` come from for a base node? | (a) the enclosing `Impl` node's owner, by span, at read time; (b) a stored column | (a). `graph.bin` has no per-file node range, so the reader scans the file's nodes, as the overlay already does for a dirty file (`view.rs:348-355`). |
| 8 | EntryPoint identity | (a) keep `compute(EntryPoint, path, None, "tag:name")`; (b) hash the handler's stored uid | (b), in area 8. Today two same-named handlers of one file share an EntryPoint uid outside the controller path (`builder.rs:2070-2073`). |
| 9 | `impact --baseline` and `peers` identity keys | (a) adopt the anchor; (b) leave them | (b) in phase 2. Neither area in this contract owns them. File a follow-up when area 5 lands. |
| 10 | C++ explicit specializations under `canonical_owner` | `Foo<int>` and `Foo<char>` become owner `Foo` | Accept. Their methods become `Later` overloads and stay distinct nodes; a target needs `@line`. |
| 11 | Cypher `ownerClass` of a scoped node | (a) stored owner (`null`); (b) the display path | (a), plus a separate parent property that area 9 names. A property that changes meaning per node kind misleads a query. |
| 12 | The `Impl`-beside-type rule of `inspect` (`inspect.rs:703-740`) | (a) keep it as a command filter before `resolve_target`; (b) drop it | (a). It is a display choice of one command, not identity. |
