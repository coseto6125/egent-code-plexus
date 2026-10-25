# C1: identity

Status: contract, phase 1. Types: `crates/ecp-core/src/contracts/identity.rs`. Landed in R2: `assign_file` numbers every file-level uid site, through `analyzer::lexical::file_identities`. Not yet adopted: the target request, the target result and the route types wait for their consuming slices.

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
| `Discriminator` | enum | `ImplTrait`, `Signature` or `Tag`: a semantic fact that separates equal keys. `stored_tag` and `from_stored` give its stored byte. |
| `FileDecl` | input row | One declaration of a file, as `assign_file` reads it. |
| `UidSlot` | result | `First(uid)`, `Later { first, ordinal, uid }` or `Duplicate { first, uid }`. |
| `Uniqueness` | result | `Unique`, or `Shared { position, count }`. |
| `DeclIdentity` | result | The slot, the anchor and the uniqueness of one declaration. |
| `IdentityError` | result | The invariant that an input breaks. |
| `TargetRequest` | value | The parsed target text plus the `--kind` and `--file` filters. |
| `Qualifier`, `Segment` | enum | The part of a target before the name. `Segment` is `Name`, `Literal` or `ImplTrait`. |
| `TargetSyntaxError` | result | Why a target text does not parse. |
| `CandidateFacts`, `PathSegment` | input row | What the resolver knows of one declaration with the requested name. |
| `FileMatch` | enum | The tier of a `--file` match: `Substring`, `Suffix`, `Equal`. |
| `TargetResolution` | result | `Unique(Candidate)`, `Ambiguous(Ambiguity)` or `NotFound { named }`. |
| `Candidate`, `Ambiguity` | result | One matching declaration; the full list when several match. |
| `RouteKey` | value | The identity of one endpoint: file, verb, composed path. |
| `HandlerRef` | enum | What serves a route: `Declaration { uid }`, `Closure { uid }`, `Inline { line }`, `Unresolved { name }`, `Absent`. |
| `RouteBinding` | value | One `HandlesRoute` fact: a route key and a handler. |

Functions: `canonical_path`, `owner_match_form`, `scoped_uid`, `assign_file`, `pair_by_anchor`, `is_scope_container`, `lexical_parents`, `file_match`, `normalize_file_needle`, `select`, `resolve_target`, `join_route_path`.

### Declaration identity: fields and invariants

| Field | Invariant |
|---|---|
| `DeclKey.path` | Repo-relative and `/`-separated, as the builder stores it; a leading `./` is part of the name. `canonical_path` gives that form. A constructor rejects any other spelling. |
| `DeclKey.owner` | The type, namespace or module that declares the node (meanings M1, M2 and M4 of design A §1.2). `Some("")` is `None`, as `uid::compute` already treats it. The text stays as the producer writes it, generic arguments and separators included. Identity never normalises it: the Rust `Impl` owners `Conv<u8>` and `Conv<u16>` are two owners and two uids (`test_unscoped_generic_owner_kept_as_produced`). |
| owner match form | `owner_match_form` removes every generic argument list and the outer whitespace: `Walk<'s, 't>` gives `Walk`. Only the target resolver uses it, to compare a request segment with a path segment. It never enters a uid. |
| `DeclKey.name` | The bare declared name, as `Node.name` stores it. |
| `DeclKey.scope` | `Unscoped`: `uid()` equals `uid::compute(kind, path, owner, name)`, today's value. `Lexical { parent_anchor }`: `owner` is `None` (I1), the kind does not keep overloads (I3), and `uid()` is `scoped_uid(parent_anchor, kind, name)`. `DeclKey::scoped` rejects `Method` and `Constructor` with `ScopedOverloadKind` (`test_scoped_method_kind_rejected`, `test_scoped_constructor_kind_rejected`). |
| `DeclKey.discriminator` | Optional. Its text is not empty. It enters the anchor and never the declaration's own uid. A scoped child hashes the parent's anchor, so adding a discriminator to a parent changes the stored uid of every scoped node below it (`test_assign_file_discriminator_changes_scoped_child_uid`). |
| discriminator storage | The variant byte is `stored_tag`: `ImplTrait` 1, `Signature` 2, `Tag` 3. A stored discriminator is the pair (byte, text); `from_stored` reads it back. |
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

A `Duplicate` row takes no part in pairing. The index keeps its first as the one node of that anchor, so the merged representative node pairs. The pairing cannot prove which `#[cfg]` variant that node stands for when two twins swap places. This is a limit of the tombstone model that Open point 4 owns; R-PI does not add it.

**Caveat rule.** A base declaration `b` of a dirty (not removed) path is hidden by ambiguity when all of these hold:

- `b` is unpaired.
- `b` has a non-empty name, so it is not a tombstone.
- Its anchor occurs at least once among that path's dirty identities.
- Its anchor occurs more than once on the base side or more than once on the dirty side.

A base declaration whose anchor is absent from the dirty side was deleted by the edit and does not count. A removed path and a tombstone never count. `OverlayView` stores the hidden declarations grouped by (path, kind, owner, name) with a count, and the view cache keeps them (`VIEW_CACHE_FORMAT` 6). Every graph command that reads the view puts one line in the payload's `result` field. The line says that the overlay could not pair N base declaration(s), names up to five groups (`Method K.save x2 (src/K.java)`), and then says `and N more`. N counts hidden declarations, never callers. The line says that an empty result is no proof of no callers, and it gives `ecp admin index --repo .` as the next step. `Engine::dirty_set_caveat` builds the line; `find` by repo adds the repo name to it.

No slice is assigned yet to remove the caveat. A `Signature` or `content_hash` slice (Open points 2 and 3) would pair an overload set, and the caveat would then shrink to the rest. FU-2026-10-25-337ab66d0277 tracks it.

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
segment   := name | literal | "<" type " as " trait ">"
literal   := "`" text "`"                    (a doubled backtick is one backtick)
line      := 1-based decimal start line
```

| Request | Matches |
|---|---|
| `load` | every declaration named `load`: any owner, scoped or not. |
| `::load` or `.load` | a `load` with no owner and no lexical parent. |
| `Engine.load`, `Engine::load` | a `load` whose qualified path ends with the segment `Engine`. |
| `a::b.f`, `a.b.f`, `a::b::f` | one request: segments `a`, `b`, name `f`. |
| `<A as Display>.fmt.pad` | the `pad` inside the `fmt` of `impl Display for A`. The type and the trait compare by their last segment. |
| `<A as Conv<u8>>.f` | the `f` of `impl Conv<u8> for A` only. A request trait with generic arguments compares the full text, whitespace aside; `<A as Conv>.f` matches every `Conv` instance (`test_qualifier_impl_trait_generic_trait_text_exact`). |
| `` `<anonymous:3:4>` ``, `` run.`<anonymous:3:4>`.helper `` | a name or a parent written as its exact stored text. Rust, Ruby, Swift, Java, JavaScript, TypeScript, Dart, PHP, C# and Kotlin name a closure node `<anonymous:line:col>` (`rust/parser.rs:335-357`, `ruby/parser.rs:547`, `swift/parser.rs:311`, `java/parser.rs:317`, `javascript/parser.rs:350`, `dart/parser.rs:305`, `typescript/parser.rs:513`, `php/parser.rs:701`, `c_sharp/parser.rs:363`, `kotlin/parser.rs:460`). Unquoted, `<anonymous:3:4>` is a bad `<Type as Trait>` segment (`test_parse_literal_name_anonymous_closure`). |
| `` `Walk<'s, 't>`.next `` | a `next` whose owner text is exactly `Walk<'s, 't>`. `Walk.next` matches the owners `Walk<'s, 't>` and `Walk` (`test_qualifier_literal_matches_exact_text_only`). |
| `pad@42` | a `pad` that starts on line 42. |
| `--kind method` | `NodeKind::from_str` of the text; an unknown kind is a syntax error. `--kind function,method` is an unknown kind: `--kind` takes one kind. |
| `--file cmd/main.go` | `\` becomes `/`, a leading `./` goes. A resolver keeps the best tier present: `Equal`, then `Suffix` (`/needle`), then `Substring`. |

| Rule | Statement |
|---|---|
| Qualifier match | The request segments equal a segment-bounded suffix of the candidate's qualified path. This holds for every language. |
| Name segment | A `Name` qualifier segment is stored in its match form and compares with the match form of the path segment, so `Walk<'s>.next` matches the owners `Walk` and `Walk<'s, 't>`. The final name keeps its text, so `operator<` stays a name. |
| Literal segment | A `Literal` segment compares its text with the path segment text, byte for byte. A literal can be the final name. An empty literal is `EmptySegment`; a missing closing backtick is `UnclosedQuote`. |
| `@` | Only `@` plus digits at the end, outside a literal, is a line. `route@fx` is a name. `` `a@1` `` is a name. `@0` is an error. |
| Render | `TargetRequest::render` joins segments with `.`, prints a root as `::name` and a line as `@N`. It writes a `Literal` segment in backticks. It writes a name in backticks when the name holds `.`, `:`, `<`, `>`, `@` or a backtick. `parse(render(r))` gives back `r`'s qualifier, name and line. |
| Reusable form | `Qualifier::of_path` writes a path segment as `Literal` when it needs backticks or its match form differs from its text, and as `Name` otherwise. So a reusable target never widens through the match form: `` `Walk<'s, 't>`.next `` selects that owner only. |
| Target filters | `--kind` and `--file` constrain target declarations in every command, including `inspect`. `inspect` aliases `--file_path` and `--file-path` have the same target semantics as `--file`. Target filters never filter the selected declaration's neighbours. |
| Neighbour filters | `inspect --neighbor-kind` filters neighbour kinds; `--neighbor-file` filters neighbour paths by substring. |

### Typed target result: `resolve_target(request, named)`

| Rule | Statement |
|---|---|
| Input | Every declaration with the requested name, from the merged graph's name index. The caller has already removed deleted declarations and synthetic nodes. |
| One match | `Unique(Candidate)`. |
| Several | `Ambiguous(Ambiguity)`. `total()` is the true count. The command never picks the first. A command that writes files refuses it. |
| None | `NotFound { named }`. `named` counts the declarations with that name before the filters: 0 means the name is not declared. |
| Reusable target | Each candidate of an ambiguity carries `reusable`: a request that selects that candidate alone among all of `named`. It escalates from the qualified name, then adds `--file`, then `@line`, then `--kind`. `None` means no target text and no filter can separate it, for example two macro-generated nodes on one line, or two overloads of one owner on one line (`test_resolve_target_overloads_on_one_line_no_reusable`). `test_reusable_render_parse_selects_only_original_every_shape` renders each reusable target, parses the text back, and asserts that it selects the original candidate only. Its shapes: a closure name, a closure parent, `Conv<u8>` beside `Conv<u16>`, `Walk<'s, 't>` beside `Walk`, a top-level and a nested `helper`, `operator<` overloads, and a backtick in a segment. |
| Bare name | A bare name matches top-level and scoped declarations. A top-level `helper` and a `helper` inside `outer` give `Ambiguous`; `::helper` and `outer.helper` each give `Unique` (`test_resolve_target_top_level_and_nested_helper_bare_name_ambiguous`). |
| Ambiguity, `inspect` | On ambiguity, `inspect` returns `status: ambiguous`, the true candidate count (`total`), and `candidates`. Each candidate contains `filePath`, `kind`, `line`, and its reusable `target` arguments (`read_context::ambiguity_candidates`). `target` is null only when the supported target syntax and filters cannot distinguish that declaration. An ambiguous response contains no full inspect blocks or neighbourhood sections. Omitted neighbourhood sections mean "not evaluated", not "empty". The response preserves applicable graph caveats (`result`). |
| Explicit kind | `--kind` filters candidates before any command display rule. `inspect A --kind impl` keeps the `Impl` node (`test_select_explicit_kind_impl_keeps_impl`); see Open point 12. |
| Old error | `Ambiguity::to_error` builds today's `EcpError::AmbiguousSymbol` (`ecp-core/src/error.rs:22-31`) with the same fields, so the message does not change when a command adopts the type. |

### Route identity and handler identity

| Field | Invariant |
|---|---|
| `RouteKey.file` | The registering file, canonical as `DeclKey.path`. |
| `RouteKey.verb` | Upper-case ASCII, not empty, no whitespace. `GRPC`, `MOUNT` and `ANY` are verbs too. |
| `RouteKey.path` | The full path with every prefix composed. `join_route_path` composes with one `/` at the joint and a leading `/`. An empty route path gives the prefix as written: `("/api", "")` is `/api`. A slash-only route path gives the prefix plus one `/`: `("/api", "/")` and `("/api/", "/")` are `/api/` (`test_join_route_path_slash_only_path_keeps_trailing_slash`). A longer route path keeps its trailing `/`. |
| `RouteKey::name` | `"<VERB> <path>"`, today's stored `Route` name. `split_name` inverts it on the first space. |
| `RouteKey::uid` | `uid::compute(Route, file, None, name)`, today's value. Both route producers call this one function. Two producers that see one endpoint make one key, so they make one node. |
| Separation | A route key never holds the handler. A handler key never holds the route. |
| `HandlerRef::Declaration { uid }` | The stored uid of the handler declaration. A route can have several bindings, and a handler can serve several routes. |
| `HandlerRef::Closure { uid }` | An anonymous function at the registration that has its own closure node: a node whose name starts with `<anonymous:` (`is_anonymous_closure`, `resolution/builder.rs:2183-2185`). `uid` is that node's stored uid. A parser emits the node only when the body holds a call (`rust/parser.rs:335-337`). It prints as `inline_anonymous`. |
| `HandlerRef::Inline { line }` | An anonymous function at the registration with no node. It prints as `inline_anonymous`. |
| `HandlerRef::Unresolved { name }` | The registration names a handler that binds no declaration. It is never printed as inline. |
| `HandlerRef::Absent` | The producer saw no handler. |

## Today

### Declaration identity

| Fact | Produced at | Read at | Maps to |
|---|---|---|---|
| uid inputs | `assign_file` through `file_identities` / `file_uids` (`ecp-core/src/analyzer/lexical.rs:44`, `:91`), one call per file; `DeclKey::uid` hashes with `uid::compute` (`ecp-core/src/uid.rs:17-27`) | builder Pass 1 (`ecp-analyzer/src/resolution/builder.rs:1096`); overlay view (`ecp-core/src/session/view.rs:378`); incremental `node_uids_for` (`ecp-cli/src/incremental.rs:285-293`); builder skip sets (`skipped_nodes`, `builder.rs:2911`); `class_membership.rs:69` | `DeclKey::unscoped(...).uid()`, equal value |
| overload ordinal | builder (`builder.rs:1119-1127`, `SymbolTable::register_overload` at `resolution/index.rs:793`); overlay (`view.rs:378-385`) | `is_later_overload` recomputes `compute` and compares (`ecp-core/src/graph.rs:1483-1500`) | `UidSlot::Later`, same ordinal rule |
| same identity, other kind | builder pushes a tombstone (`builder.rs:1100-1118` comment, D1 collision recovery) | the tombstone has an empty name and no symbol-table entry | `UidSlot::Duplicate`, unchanged; every tombstone of one base uid stores the same `uid::tombstone(base)`, so the uid does not separate them; see Open point 4 |
| "keeps overloads" | `uid::keeps_overloads` (`uid.rs:33-35`): `Method`, `Constructor` | builder, view | used as is |
| overlay pairing | `dirty_base_by_uid` (`view.rs:348-355`), lookup at `view.rs:388`: by stored uid, so by position for overloads | `ViewNode.replaced_base` (`view.rs:254`) | `pair_by_anchor`; a `Shared` anchor pairs nothing |
| owner (M1, M2, M4) | parsers; `stamp_owner_class_by_span` (`ecp-analyzer/src/framework_helpers.rs:260`); Rust `enclosing_impl_type` (`rust/receiver_types.rs:286`), which strips the self type's generics; Rust `Impl` owner is the raw trait text, generics included (`rust/parser.rs:438-444`, `impl_trait_name` at `rust/receiver_types.rs:113-118`); Rust `Typedef` in an impl takes the same trait text (`rust/parser.rs:446-453`, `enclosing_impl_or_trait_context` at `rust/receiver_types.rs:258-273`) | `Node.owner_class` (`graph.rs:698-702`) | `DeclKey.owner`, text unchanged |
| owner spelling | `PathLiteral` owner is the enclosing owner verbatim (`post_process/path_literal_nodes.rs:45-70`), so `Walk<'s, 't>` sits beside the function owner `Walk` (review page, postparse F1); C++ `Foo<int>::m` takes owner `Foo` (`cpp/parser.rs:526-534`, `scope_type_name` at `cpp/receiver_types.rs:207-212`) | target lookup | identity keeps each text; the resolver compares by `owner_match_form` |
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
| unbound occurrence, `rename` | the rename edits the bound sites, leaves the rest, and exits 0 | — | A write run refuses and writes no file when any occurrence is unbound. `--dry-run` lists it. `--leave-unbound` writes the bound sites and leaves the rest. The flag never waives an ambiguity or a warm-attach refusal. |
| not found | `InvalidArgument("No symbol named …")` (`impact/symbol.rs:143-148`); `inspect` `status: error` (`inspect.rs:695-701`) | agent | `NotFound { named }` |

### Route and handler identity

| Fact | Produced at | Read at | Maps to |
|---|---|---|---|
| Route uid, decorator producer | `uid::compute(Route, path, None, "VERB path")` (`builder.rs:1407-1411`) | `ecp routes` (`commands/routes.rs`) | `RouteKey::uid`, equal value |
| Route uid, call producer | same expression (`builder.rs:1477-1481`) | same | `RouteKey::uid`, equal value |
| duplicate Route nodes | NestJS gives two `Route` nodes with one uid (review page, frameworks F2; producer pair not traced) | `ecp routes` prints two rows | one key gives one node |
| route name split | `split_route_name` (`commands/routes.rs:201`); `diff/routes.rs:77` | routes, diff | `RouteKey::split_name` |
| verb case | `route_detector.rs:225` upper-cases | — | `RouteKey::new` upper-cases |
| prefix join | `join_route_path` (`ecp-analyzer/src/route_detector.rs:353-371`) drops every empty segment and every trailing `/`: `["/api", "/"]` gives `/api`, `["/api", "users/"]` gives `/api/users` | Laravel group routes | C1 `join_route_path` keeps a slash-only or trailing `/`. A grouped route written as `/` or with a trailing `/` changes its path and its Route uid; the `RouteKey` row of Stored data bumps the fingerprint for it |
| handler, decorator | the decorated node (`builder.rs:1403`, edge at `:1421-1427`) | `routes.rs` incoming `HandlesRoute` | `HandlerRef::Declaration` |
| handler, call | `RawRoute.handler: Option<String>` (`ecp-core/src/analyzer/types.rs:507`), bound by `lookup_in_file`, first node of that name (`builder.rs:1508`) | — | `HandlerRef::Declaration` through C4's endpoint; `Unresolved` when nothing binds |
| handler kinds printed | `named` (`routes.rs:478`), `unresolved_action` (`:424`), `inline_anonymous` for every route with no edge (`:446`) | agent | `Declaration` → `named`; `Unresolved` → `unresolved`; `Closure` and `Inline` → `inline_anonymous`; `Absent` → `absent` |
| C4 handler endpoint | C4 maps `Node(i)` to `Declaration`, `Unresolved(Named)` to `Unresolved`, `Unresolved(Anonymous)` and `Unresolved(Inline(_))` to `Inline { line }` and no handler to `Absent` (`C4-raw-records.md:230-235`) | — | `Node(i)` whose node is a `<anonymous:` closure → `Closure { uid }`; any other `Node(i)` → `Declaration { uid }`; settles C4 Open point 5 below |
| EntryPoint uid | `compute(EntryPoint, path, None, "tag:handler_name")` (`builder.rs:2067-2073`); controller actions add the owner (`builder.rs:1737-1746`) | entry-point readers | not mapped; see Open point 8 |

## Consumers

| Area | Reads | Must stop |
|---|---|---|
| 1 (resolution) | `pair_by_anchor` for `replaced_base`; `lexical_parents` as the lexical tier input on both sides; "scoped means not a global candidate" for the global filter | pairing by stored uid (`view.rs:388`); the Rust lexical scope strings in `index.rs:196-203` and `same_file_pick.rs:545`; `has_owner` as the lexical admission input |
| 2 (owner split) | `DeclKey`, `Scope`, `Discriminator` with `stored_tag`, `assign_file`, `lexical_parents` | writing M3 strings into `owner_class`; tombstoning a repeated scoped node; hashing a parent's positional uid; printing an owner the target syntax cannot read back |
| 5 (read path) | `TargetRequest::parse`, `resolve_target`, `TargetResolution`, `Ambiguity::to_error` | seven resolvers; the Rust-only suffix match; `rename` renaming every match; `inspect` returning full blocks for an ambiguity; a bare `rename` meaning top-level only |
| 8 (routes) | `RouteKey`, `join_route_path`, `HandlerRef`, `RouteBinding` | two `uid::compute(Route, …)` sites; one node per producer for one endpoint; a handler bound by first-name lookup; `inline_anonymous` for a named handler that did not bind |
| 9 (Cypher) | node identity is the merged node index: `DISTINCT` on a node keys on it (`ecp-core/src/cypher/executor.rs:462-468`, `value.rs:53-57`); the `uid` property is `UidSlot::uid` (`executor.rs:2816`) | deduplicating nodes by name; reading `ownerClass` (`executor.rs:2817`) as an identity |

### Cross-area conflicts that name C1

| Pair | Requirement | How C1 meets it |
|---|---|---|
| 1, 2 | the global candidate filter (`resolution/index.rs:1073`, now `:1059-1081`), the lexical input, the uid pairing (`view.rs:388`) | one `lexical_parents` gives both the filter fact and the lexical input; `pair_by_anchor` replaces uid pairing. The order of the slices stays as the plan's Order section states. |
| 2, 5 | the target resolver reads the owner (`symbol_id.rs:141`); `rename` treats an empty owner as top level (`rename.rs:332-345`) | the resolver reads the qualified path (owner plus parents), never the raw owner. A bare name matches top-level and scoped declarations; a tie is an ambiguity that `rename` refuses. The top-level meaning has its own syntax, `::name` (Open point 5, adopted). |
| 2, 8 | Route uids made in two places (`builder.rs:1411`, `:1481`) | `RouteKey::uid` is the one constructor. It hashes no handler and goes through no `assign_file` numbering, so a uid change of area 2 does not move a Route uid. |

## Stored data

| Change | `GRAPH_FORMAT_VERSION` (15, `graph.rs:36`) | `BUILDER_FINGERPRINT` (`schema25`, `registry/commit_meta.rs:93`) | L1 fragment (`FRAGMENT_FORMAT_V5`, `session/overlay.rs:57`) | dirty manifest | view cache key (`VIEW_CACHE_FORMAT` 4 since R1b, `session/view.rs`) |
|---|---|---|---|---|---|
| adopt `DeclKey` for unscoped nodes, no discriminator, owner text as produced | none | none | none | none | none |
| `assign_file` in the four paths that skip ordinals (A-A) | none | bump | none | none | follows |
| the parent link table (A-B) | bump | bump | none | none | follows |
| a language opts in to scoped uids (A-C, A-D, A-E): every scoped node of that language changes its stored uid | none | bump | none: `FragmentSymbol` has kind, span and owner (`ecp-cli/src/session/overlay_writer.rs:85-96`) | none | follows |
| a repeated scoped node, and a `#[cfg]` twin under Open point 4, gets the overload uid `uid::overload(base, ordinal)`, not a tombstone | none | bump | none | none | follows |
| a producer changes its owner text (for example drops the generic arguments of the `PathLiteral` owner or of the Rust `Impl` and `Typedef` owners): the unscoped uids of those kinds change. C1 does not require it, because the resolver matches by `owner_match_form` | none | bump | none | none | follows |
| `Discriminator::ImplTrait` lands in or before the slice where Rust opts in to scoped uids | none | none: an unscoped uid does not hash it, and no scoped child exists yet | none: the `Impl` nodes are in the fragment | none | follows |
| `Discriminator::ImplTrait` lands after Rust opts in: the stored uid of every scoped node under a trait-impl member changes | none | bump | none | none | follows |
| `Discriminator::Signature` or `Tag`: a new stored table `discriminators: Vec<DiscriminatorFact { node_idx: u32, tag: u8, text: StrRef }>` in `ZeroCopyGraph`, appended at the end of the archived struct, `tag` from `stored_tag` | bump | bump (new parser fact; also changes scoped child uids when the language already opts in) | bump: `FragmentSymbol` lacks the fact | none | follows |
| `pair_by_anchor` in the overlay | none | none | none | none | follows: the binary path, size and mtime are in the key (`ecp-cli/src/session/view_cache.rs:36-43`) |
| `RouteKey` with composed prefixes; one node per key | none | bump | none | none | follows |
| `TargetRequest` / `resolve_target` | none | none | none | none | none |

Not every stored uid stays as today. Three changes move uids, and each one bumps `BUILDER_FINGERPRINT` in the slice that makes it: a scoped node takes a uid from its parent's anchor; a repeated node takes the overload uid; a producer that changes its owner text changes the unscoped uids of its kinds. Only an unscoped declaration whose owner text and kind do not change keeps today's uid.

The `ImplTrait` text for a base node comes from the enclosing `Impl` node's owner at read time (Open point 7), so it needs no stored column. `Signature` and `Tag` have no node to read them from, so they need the `discriminators` table above.

## Open points

| # | Decision | Options | Recommendation |
|---|---|---|---|
| 1 | Does the discriminator enter the stored uid? | (a) anchor only; (b) the uid too | (a). The declaration's own uid stays as today for every Rust trait-impl method. The uids of scoped nodes below it still change, because they hash the anchor: see the `ImplTrait` rows of Stored data. (b) changes uids of nodes that have no defect. |
| 2 | Who supplies `Discriminator::Signature`? | (a) a parser field per language; (b) none, and `Shared` stays | (b) for now. Java, Kotlin, C#, C++, Swift and Dart overloads stay `Shared`, so they pair nothing and need `@line` as a target. Two overloads of one owner on one line cannot be told apart by `@line` either; they get no reusable target (`test_resolve_target_overloads_on_one_line_no_reusable`). Add (a) only when a measured case needs it; (a) needs the `discriminators` table and a `GRAPH_FORMAT_VERSION` bump. |
| 3 | What pairs a `Shared` group in the overlay? | (a) nothing; (b) equal `content_hash` first, then the one remaining pair | (a) in the first slice; the view suppresses the base members and the caveat names them. (b) needs a per-symbol hash in `FragmentSymbol` (L1 bump). `impact --baseline` already pairs closures by body hash (`impact/baseline.rs:408-444`), so (b) has a precedent. |
| 4 | A `Duplicate` of a kind that does not keep overloads (a `#[cfg]` twin) | (a) tombstone, as today; (b) `Later` for every kind | (b), after `is_later_overload` (`graph.rs:1483`) stops recomputing `compute` and reads the slot. The twin stays findable; the review counts 15 hidden `Function` tombstones in this repo. It takes the overload uid, a stored-uid change. Owner: area 2. |
| 5 | Does a bare name match scoped nodes? | (a) yes, and a tie is an ambiguity; (b) no, as design A §5.5 proposes for `rename` | Adopted: (a). One rule for every command. `rename helper` with a top-level and a nested `helper` refuses the ambiguity and lists `::helper` and `outer.helper` (`test_resolve_target_top_level_and_nested_helper_bare_name_ambiguous`). This departs from design A §5.5. The plan's conflict row 2-5 states it. |
| 6 | The scope of a route key | (a) the registering file, as today; (b) the service root | (a). It keeps today's uids. Two services in one monorepo stay apart when their route files differ. |
| 7 | Where does `ImplTrait` come from for a base node? | (a) the enclosing `Impl` node's owner, by span, at read time; (b) a stored column | (a). The text is the full trait text as `impl_trait_name` produces it, generic arguments included (`Conv<u8>`), so `impl Conv<u8> for A` and `impl Conv<u16> for A` get two anchors. `graph.bin` has no per-file node range, so the reader scans the file's nodes, as the overlay already does for a dirty file (`view.rs:348-355`). |
| 8 | EntryPoint identity | (a) keep `compute(EntryPoint, path, None, "tag:name")`; (b) hash the handler's stored uid | (b), in area 8. The tag stays part of the key: the key hashes the tag plus the handler's stored uid, so one handler with two entry-point tags keeps two EntryPoint nodes. Today two same-named handlers of one file share an EntryPoint uid outside the controller path (`builder.rs:2070-2073`). |
| 9 | `impact --baseline` and `peers` identity keys | (a) adopt the anchor; (b) leave them | (b) in phase 2. Neither area in this contract owns them. File a follow-up when area 5 lands. |
| 10 | C++ explicit specializations | `Foo<int>::m` and `Foo<char>::m` | No change. The verified producer is the C++ `Method` path: `scope_type_name` (`cpp/receiver_types.rs:207-212`) already gives owner `Foo` (`cpp/parser.rs:526-534`), so these methods are `Later` overloads today and stay so. A target needs `@line`. Other C++ kinds were not traced. |
| 11 | Cypher `ownerClass` of a scoped node | (a) stored owner (`null`); (b) the display path | (a), plus a separate parent property that area 9 names. A property that changes meaning per node kind misleads a query. |
| 12 | The `Impl`-beside-type rule of `inspect` (`inspect.rs:703-740`) | (a) keep it as a command filter before `resolve_target`; (b) drop it | (a). It is a display choice of one command, not identity. An explicit `--kind` wins over it: `inspect A --kind impl` keeps the `Impl` node, and the rule applies only when no `--kind` is given. |
| 13 | C4 Open point 5: a closure handler | (a) a closure node is a `Node` endpoint; (b) always an unbound inline handler | (a), with `HandlerRef::Closure { uid }`. C4 reads a `Node(i)` endpoint whose node name starts with `<anonymous:` as `Closure { uid }`, any other `Node(i)` as `Declaration { uid }`, and `Unresolved(Anonymous)` or `Unresolved(Inline(_))` as `Inline { line }`. `Closure` and `Inline` both print `inline_anonymous`, so `ecp routes` output keeps its values. The uid lets `impact` reach the calls inside the closure. |
