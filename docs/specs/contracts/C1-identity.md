# C1: identity

Status: contract. Declaration identity is implemented through phase 2 (the container chain); phases 3 and 4 are open (Open points 14 and 17). Types: `crates/ecp-core/src/contracts/identity.rs`. Landed in R2: `assign_file` numbers every file-level uid site, through `analyzer::lexical::file_identities`. Not yet adopted: the target request, the target result and the route types wait for their consuming slices.

Plan row (`docs/specs/2026-10-25-architecture-direction.md`, "Contracts before code"): declaration identity, lexical parent, the target request syntax and a typed ambiguity result; route identity and handler identity as separate keys. Consumers: areas 1, 2, 5, 8, 9.

Most `file:line` below were read on branch `feat/arch-p1-contracts` at `1d267693`, and lines may have moved since. The sections Repeat policy, Stability against unrelated edits, Collision shapes per language and Edge sources bind by row, and the Stored data constants, were read at `57fb9d46`.

## Defines

### Terms

| Term | Meaning |
|---|---|
| base uid | The hash of a declaration key before overload numbering: `DeclKey::uid`. |
| stored uid | The uid a node carries in `graph.bin` and in the overlay: `UidSlot::uid`. |
| anchor | The semantic identity of a declaration. It equals the base uid: `DeclKey::anchor` returns `uid()`. Pairing, `diff` and child uids read it. |
| scoped | A declaration whose lexical parent is a callable. It has no owner. |
| scope container | A declaration of a kind that `is_scope_container` accepts. It can hold other declarations. |
| container chain | The scope containers of a declaration, innermost first. A member's uid hashes its container's anchor, which hashes its own container's anchor, and so on. |
| qualified path | The owner segments of a declaration, then the names of its lexical parents, outermost first. |

### Types

| Type | Kind | Purpose |
|---|---|---|
| `DeclKey` | value | The identity inputs of one declaration: kind, path, owner, name, scope. |
| `Scope` | enum | `Unscoped`, `Member { parent_anchor }` (a declaration that a scope container declares), or `Lexical { parent_anchor }`. |
| `FileDecl` | input row | One declaration of a file, as `assign_file` reads it: its key fields, its lexical `parent` and its `container`. |
| `UidSlot` | result | `First(uid)`, `Later { first, ordinal, uid, reason }` or `Duplicate { first, uid }`. |
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

Functions: `owner_match_form`, `scoped_uid`, `nested_uid`, `assign_file`, `pair_by_anchor`, `is_scope_container`, `lexical_parents`, `container_parents`, `file_match`, `normalize_file_needle`, `select`, `resolve_target`, `join_route_path`.

### Declaration identity: fields and invariants

| Field | Invariant |
|---|---|
| `DeclKey.path` | Repo-relative and `/`-separated, as the builder stores it; a leading `./` is part of the name. `ecp_core::registry::os_path_to_slash` gives that form: `\` becomes `/` on Windows only, and stays a file-name character on Unix. A constructor rejects a `\` on Windows only. |
| `DeclKey.owner` | The type, namespace or module that declares the node (meanings M1, M2 and M4 of design A §1.2). `Some("")` is `None`, as `uid::compute` already treats it. The text stays as the producer writes it, generic arguments and separators included. Identity never normalises it: the Rust `Impl` owners `Conv<u8>` and `Conv<u16>` are two owners and two uids (`test_unscoped_generic_owner_kept_as_produced`). |
| owner match form | `owner_match_form` removes every generic argument list and the outer whitespace: `Walk<'s, 't>` gives `Walk`. Only the target resolver uses it, to compare a request segment with a path segment. It never enters a uid. |
| `DeclKey.name` | The bare declared name, as `Node.name` stores it. |
| `DeclKey.scope` | `Unscoped`: `uid()` equals `uid::compute(kind, path, owner, name)`, today's value; only a top-level declaration (no container) that joins no type is unscoped. `Member { parent_anchor }`: `uid()` is `nested_uid(parent_anchor, kind, owner, name)`, with `parent_anchor` the anchor of the declaration's container. `Lexical { parent_anchor }`: `owner` is `None` (I1), the kind does not keep overloads (I3), and `uid()` is `scoped_uid(parent_anchor, kind, name)`. `DeclKey::scoped` rejects `Method` and `Constructor` with `ScopedOverloadKind` (`test_scoped_method_kind_rejected`, `test_scoped_constructor_kind_rejected`). |
| `scoped_uid` stream | Starts with byte `0x01` and is at least 14 bytes long. A `compute` stream starts with an ASCII letter. An `overload` stream is 12 bytes. No two streams are equal. |
| `nested_uid` stream | `0x03`, the parent anchor (8 bytes, little-endian), then `kind \0 owner \0 name`: at least 15 bytes. The owner stays in the stream, so two members of one container that differ only by owner keep two uids (`test_member_key_uid_is_nested_uid_and_keeps_owner`, `test_stream_tags_disjoint_from_kind_text`). |
| anchor stream | None. The anchor equals the base uid. Stream tag `0x02` is retired: a reuse needs a `BUILDER_FINGERPRINT` bump. |

### File assignment: `assign_file(path, decls)`

| Rule | Statement |
|---|---|
| Input | One `FileDecl` per declaration of one file, in source order. `parent` comes from `lexical_parents`, `container` from `container_parents`. A parent or container can come before or after its children. |
| Anchors first | A child's key hashes its parent's anchor, or else its container's anchor, so the function resolves each chain (`parent`, else `container`) before the child. |
| Key | A row with a `parent` is `Lexical`. A row with no `parent` and a `container` is `Member`. A row outside every type (at top level, or in a namespace or module) of a kind that keeps overloads or merges repeats joins a type that declares a member of its kind, owner and name, and is `Member` too (`joined_containers`): a C++ out-of-line `K::m` or `int K::x = 0;` after the in-class one, a Swift `extension P { var af }` beside the `var af` of protocol `P`. It is then a later overload or a merged repeat of that member (`test_assign_file_out_of_type_member_joins_in_type_sibling_set`). The join needs one such type, or several of one anchor; the type lies inside the row's own container and outside every callable. A method joins in either order; a field joins only an earlier in-type sibling (`test_assign_file_out_of_type_field_before_its_sibling_joins_none`). Every other row with neither is `Unscoped` and keeps its `uid::compute` uid. |
| Errors | A parent or container out of range or a cycle gives `BadParent`. A scoped row with an owner gives `ScopedWithOwner`. A scoped row of a kind that keeps overloads gives `ScopedOverloadKind`. |
| `UidSlot` | Groups by base uid in source order. The first keeps the base uid. A later one gets `uid::overload(base, ordinal)` with a 1-based ordinal. The first match sets the slot: a kind that keeps overloads is `Later { reason: Overload }`; a scoped one is `Later { reason: Scoped }`; a C# declaration whose arity path no earlier one has is `Later { reason: Arity }`; a kind that merges its repeats (`uid::merges_repeats`: `Property`, `Variable`, `Const`) is `Duplicate`; any other kind is `Later { reason: Repeat }`. A `Duplicate` keeps the tombstone uid `uid::tombstone(uid::overload(base, ordinal))`, so no two tombstones share a uid. |
| `Uniqueness` | Groups by anchor. One member: `Unique`. Several: `Shared { position, count }` for each. |
| Positional rule | A `Shared` position proves nothing about which declaration an edit kept. It never pairs a node, and it never forms a target alone. |

Example: `impl Display for A { fn fmt { fn pad } }` then `impl Debug for A { fn fmt { fn pad } }`. Each `fmt` has its `Impl` block as container, and the two blocks have two uids, because the `Impl` owner is the trait. So each `fmt` is `First` with its own `nested_uid`, and each `pad` hashes its own `fmt` anchor. Swap the two blocks, and every member keeps its uid. The test `test_assign_file_swapped_impls_keep_child_uids` asserts it.

Example: `class O1 { class H { run } }` and `class O2 { class H { run } }`. Each `H` has a different container, so the two `H` and the two `run` are `First`, with two uids each. Two same-named containers in one scope (`impl S {}` twice) share an anchor, so their members share a parent anchor and number as one group (`test_assign_file_repeated_container_members_share_parent_anchor`).

Example: one function holds two blocks, and each block declares `h`. Both `h` rows have one parent anchor, so they share an anchor: `Shared`. The second gets `Later`, not `Duplicate`, so both stay nodes. The test `test_assign_file_sibling_blocks_shared_not_duplicate` asserts it.

### Repeat policy

Two mechanisms keep same-named declarations apart. Each fits one shape.

| Shape | Mechanism | Why |
|---|---|---|
| Same scope, same key: a redefinition, an `#ifdef`, `#[cfg]` or `#if` twin, a free-function overload, a Go `init` | An ordinal: `Later` with `uid::overload(base, n)`. | Only order separates them. |
| Different scopes with equal short owners: nested types, namespaces and modules at depth 2 or more, locals in two callables | The container chain: `nested_uid` or `scoped_uid` hashes the container's anchor. | An ordinal depends on order, so an earlier insert renumbers later declarations. It also puts different entities in one overload set, and that gives false callers. |

The ordinal therefore serves same-scope, same-key repeats only: redefinitions and overloads. A nested path takes the container chain.

A repeat of a base uid takes one of two classes.

| Class | Kinds | A repeat becomes | Why |
|---|---|---|---|
| Keeps its own node | Every kind not listed below: `Function`, `Class`, `Struct`, `Enum`, `Interface`, `Trait`, `Impl`, `Namespace`, `Module`, `Typedef`, `Macro`, `Section`, `EnumVariant`, `Annotation` and the rest. | `Later` with `uid::overload(base, n)`. It keeps its name and owner, and it joins the overload set (`register_overload`). | Each body holds its own calls and members. A call binds the first by name. A `References overload_set` edge reaches the rest. |
| Merges | `Property`, `Variable`, `Const`. | `Duplicate`, with the tombstone uid `uid::tombstone(uid::overload(base, n))`. | One field assigned in N methods, a JS `var x` twice, or a Python `x = 1; x = 2` is one entity. `field-reassign` merges it without a `BlindSpot` outside C and C++ files; a merge in a C or C++ file records `ifdef-redef`. |

| Rule | Statement |
|---|---|
| Predicate | `uid::merges_repeats(kind)` is true for `Property`, `Variable` and `Const`. Three places read it: the repeat decision in `assign_file`, `joins` in `joined_containers` (a merging kind can join an in-type sibling), and the overlay scan of base twins in `session/view/resolution.rs`. |
| Overload kinds | `uid::keeps_overloads` keeps its meaning. It never widens: Rust, Python, JS and TS nested functions would lose their lexical parent, and bare-call resolution would change. |
| Slot reason | The `UidSlot` row of File assignment lists the reasons. A kind that keeps its own node and repeats is `Later { reason: Repeat }`. Its `first` is the first declaration of the same arity path. |
| Container kind | A scope container is never a `Duplicate`. The overlay derives a child's anchor from its parent's stored name, and a tombstone stores an empty name. |
| Tombstone | The overlay writes a tombstone for a merged row (`Duplicate`) only. Pass 1 writes one for a merged row (`Tombstone::MergedRepeat`) and for a true 64-bit uid collision with a declaration of another path (`Tombstone::Collision`, `BlindSpot` `uid-collision`). A `Later` row has its own node and records no `BlindSpot`. |
| Own-node edges | A `Later` row has its own node, so its `Defines`, `Calls` and membership edges start and end at that node (see Edge sources bind by row). |
| Two inputs of one path | `lexical::path_identities` numbers the inputs of one path as one file: a declaration of a later input repeats the earlier inputs' ones. Spans relate only rows of one input. |
| C# same-file `partial` | A second `partial class P` in one file is `Later` and keeps its name. `PartOf` links it to the representative. Its members belong to its own part. |
| Overlay `partial` | The two parts share one anchor, so a dirty file pairs neither. The overlay replays no `PartOf` for them, and the `Shared` caveat names `P`. This is a named exception (Open point 16). |

### Stability against unrelated edits

| Edit | Uid change |
|---|---|
| Insert, delete or move a declaration elsewhere in the file | None. No position enters a key, and an ordinal counts only earlier declarations of the same base uid. One exception: an out-of-type field joins only an earlier in-type sibling, so moving it before the sibling, or deleting the sibling, changes its uid. |
| Edit a body: spans change, nesting does not | None. |
| Insert an earlier declaration with the same key, such as a third `def f` above | The later ones of that key renumber. Only a semantic fact could avoid it (Positional rule). |
| Rename or re-nest a container | The uids of its descendants change. Their path changed. |
| Swap two blocks that each hold a same-named type (`O1 { H }`, `O2 { H }`) | None. Each chain hashes its own container. |
| Swap two blocks of one name with equally named members (`impl S` twice) | The members renumber. The two blocks share one anchor, so the members share a parent anchor and number by position (Positional rule). |

Two example tests cover it: `test_file_uids_unrelated_insert_and_block_swap_keep_other_uids` in `analyzer/lexical.rs`, and the block-swap case in `ecp-cli/tests/uid_assign_parity.rs`. No property test over many fixtures exists. Such a test would apply one unrelated edit per fixture, derive the identities again, and assert that every other uid is equal.

### Collision shapes per language

Each shape code names one way that two declarations can collide. A test that stands for a shape cites its code.

| Code | Shape |
|---|---|
| R | A same-scope redefinition: conditional, `#ifdef`, `#[cfg]`, `#if`, or a plain repeat. |
| FO | A free-function overload. |
| N1 | A same-named type at depth 1 (`O1.H`, `O2.H`): the members collide. |
| N2 | A same-named type at depth 2 or more (`O1.M.H`, `O2.M.H`): the type collides. |
| NS | A same-named member of two namespaces or modules at depth 2 or more, or with no owner. |
| L | A same-named local declaration in two different callables. |
| M | One entity declared in parts: a partial, a merge, a reopen. |
| V | A variable or field reassign (the merge class). |
| GS | A getter and setter pair. |

Every shape gives named nodes with distinct stored uids, with two exceptions: V gives one node, and a Dart getter and setter (GS) give one merged `Property`. A cell below holds the fixture of the shape, and a test runs it. `yes` means the shape exists and its fixture is the one of the other languages (`O1.H` and `O2.H`, `A.B.H` and `C.B.H`). A dash means the shape does not exist in that language, or the parser has no construct for it. `open` means a test marked `#[ignore]` shows that the shape still collides (Open point 14). `unverified` means no test runs the shape.

| Language | R | FO | N1 | N2 | NS | L | M | V | GS |
|---|---|---|---|---|---|---|---|---|---|
| Python | `def f` twice; `def f` in both `if/else` branches | — | yes | yes | — | nested `def` in two methods | — | merge: `x=1; x=2` | unverified: `@property` and `@x.setter` |
| JavaScript | `function f` twice | — | open: a class expression has no `Class` node | unverified | — | nested `function` in two methods | — | merge: `var x` twice | unverified |
| TypeScript | `function f` twice | unverified | open: a class beside a `namespace` class of the same name | unverified | open: no `Namespace` node (`namespace N { namespace M { f } }` twice) | nested `function` in two methods | `interface I` twice | unverified | unverified |
| Java | — | — | yes | yes | — | local `class L` in two methods | — | unverified | — |
| Kotlin | `fun f` twice | `fun f(Int)`, `fun f(String)` | yes | yes | — | local `fun l` in two funs | — | unverified | — |
| C# | `#if` twin classes | — | yes | yes | unverified | unverified | same-file `partial class P` twice | unverified | unverified |
| Go | `func init` twice | — | — | — | — | — | — | unverified | — |
| Rust | `#[cfg]` fn twins; a repeated inherent `impl X` | — | `impl S` in two modules | — | `o1::inner::f`, `o2::inner::f` | local `const X` in two nested `helper`s | — | — | — |
| PHP | conditional `function g` | — | — | — | unverified | nested `function l` in two functions | — | unverified | — |
| Ruby | `def f` twice (kind `Method`) | — | yes | yes | — | unverified | a reopened class | — | unverified |
| Swift | — (a same-signature repeat parses as FO) | top-level `func f(Int)`, `func f(String)` | yes | yes | — | local `func l` in two functions | a method in two `extension K` blocks | a field in `extension P` joins the requirement of protocol `P` | unverified |
| C | `#ifdef` twins of `int f` and of `struct S` | — | — | — | — | local `struct L` in two functions, with its field | — | a field and a variable across `#if`, `#elif`, `#else` | — |
| C++ | `#ifdef` twins of `int f()` | `void g(int)`, `void g(double)` | yes | yes | `n::i::f`, `m::i::f` | local `struct L` in two functions | — | an out-of-line `int K::x = 0;` merges into the in-class `x` | — |
| Dart | — | — | — | — | — | local function `l` in two functions | — | — | getter and setter: one merged `Property` |

A Rust associated `type X` in `impl Trait for A` and in `impl Trait for B` is two `Typedef` nodes: the two `Impl` blocks are two containers.

Every cell with a fixture has a test. A test cites the shape code and this section.

### Overlay pairing: `pair_by_anchor(base, dirty)`

A dirty declaration replaces a base declaration of the same file only when their anchor occurs exactly once on each side. Every other dirty declaration is unpaired. An unpaired base declaration is suppressed in the dirty view, as a deleted one is.

A `Duplicate` row (a merged `Property`, `Variable` or `Const` repeat) takes no part in pairing. The index keeps its first as the one node of that anchor, so the merged representative node pairs. The pairing cannot prove which repeat that node stands for when two repeats swap places. This is a limit of the merge; R-PI does not add it.

**Caveat rule.** A base declaration `b` of a dirty (not removed) path is hidden by ambiguity when all of these hold:

- `b` is unpaired.
- `b` has a non-empty name, so it is not a tombstone.
- Its anchor occurs at least once among that path's dirty identities.
- Its anchor occurs more than once on the base side or more than once on the dirty side.

A base declaration whose anchor is absent from the dirty side was deleted by the edit and does not count. A removed path and a tombstone never count. `OverlayView` stores the hidden declarations grouped by (path, kind, owner, name) with a count, and the view cache keeps them (`VIEW_CACHE_FORMAT` 15). Every graph command that reads the view puts one line in the payload's `result` field. The line says that the overlay could not pair N base declaration(s), names up to five groups (`Method K.save x2 (src/K.java)`), and then says `and N more`. N counts hidden declarations, never callers. The line says that an empty result is no proof of no callers, and it gives `ecp admin index --repo .` as the next step. `Engine::dirty_set_caveat` builds the line; `find` by repo adds the repo name to it.

No slice is assigned yet to remove the caveat. A `Signature` or `content_hash` slice (Open points 2 and 3) would pair an overload set, and the caveat would then shrink to the rest. FU-2026-10-25-337ab66d0277 tracks it.

### Container: `container_parents(decls)`

| Rule | Statement |
|---|---|
| Input | `(kind, span)` per declaration of one file. |
| Link | The innermost declaration that strictly contains the row and whose kind passes `is_scope_container`. Identical spans do not contain each other. |
| Property | A `Property` row skips callable containers (`Function`, `Method`, `Constructor`), so `self.x` assigned in two methods has the class as container and merges. A `Variable` or `Const` keeps its callable container. |
| Output | `Some(index)` in the same slice, or `None` at the top level. |
| Callers | `identities_of` in `ecp-core/src/analyzer/lexical.rs`, so the index build, the overlay view, the overlay reader and incremental mode all read it; the membership owner pass reads it for a member of an `Impl` block. |

### Lexical parent: `lexical_parents(decls, scoped_kinds)`

| Rule | Statement |
|---|---|
| Input | `(kind, span)` per declaration of one file, plus the language's opt-in kind list (design A §5.2). The list is a parameter, so this contract does not depend on C5's language value. |
| Containers | `Function`, `Method`, `Constructor`, `Class`, `Struct`, `Enum`, `Interface`, `Trait`, `Impl`, `Namespace`, `Module` (`is_scope_container`). |
| Link | The innermost container that strictly contains the node, by `innermost_enclosing` (`ecp-core/src/analyzer/types.rs:40`). A non-container ancestor between them is skipped. |
| Scoped | The kind is in the opt-in list, the kind does not keep overloads, and that container is a callable. A class or module in between breaks the link. |
| Output | `Some(index)` of the callable in the same slice, or `None`. An empty opt-in list gives `None` for every node. |
| One derivation | The parser, the index build, the overlay view, the overlay reader and incremental mode call this function. No consumer builds a lexical scope string. |

### Scope occurrence identity

Status: contract for a future slice, with C4 "Scope table and import bindings", which defines the scope occurrence, `ScopeId` and the import binding. No code reads it today.

| Rule | Statement |
|---|---|
| Not a declaration | A scope occurrence is a source region, not a declaration. It never enters a uid, so the scope table changes no stored uid. |
| `ScopeId` | Snapshot-local. It is valid only inside the raw graph, the L1 fragment or the `graph.bin` file range that wrote it. A key that outlives that snapshot never holds a `ScopeId`. |
| Scope key | The stable identity of an occurrence. For `File`, it is the file path. For an occurrence with `decl: Some(i)`, it is the anchor of node `i` when that anchor is `Unique` (File assignment). A `Block`, or a body whose anchor is `Shared`, has no scope key. |
| Direction | An occurrence takes the anchor of its declaration. A declaration takes nothing from an occurrence. |
| Namespace block | The anchor names the namespace. Each block of the namespace is its own occurrence. Two blocks of `namespace N` in one file share one anchor, so neither has a scope key (Positional rule). |
| Cross-occurrence visibility | Identity never decides visibility. The language rule of C4 "Scope lookup" decides whether one block sees the bindings of another block of the same anchor. In C# and PHP it does not. In C++ a later block sees what an earlier block of the namespace declares before it. The walk links the blocks through their shared anchor, not through a scope key. |
| Lexical parent | The scope table agrees with `lexical_parents`. For a declaration with lexical parent `p`, the chain from its occurrence reaches the `Callable` occurrence of `p` through `Block` occurrences only. A `Type` or `Module` occurrence on that path means no lexical parent. A test derives both from one fixture and compares them. |
| Overlay pairing | `pair_by_anchor` pairs declarations only. The overlay never pairs a scope occurrence or an import binding. A dirty file's scope table and bindings replace the whole set of the base file. |
| Cache | A cache entry keyed by a `ScopeId` lives no longer than the view generation that built it. |

| Edit | `ScopeId` | Scope key |
|---|---|---|
| Insert whitespace, or edit a body and add no occurrence | unchanged | unchanged |
| Insert an unrelated occurrence | every later id moves | unchanged |
| Swap two sibling subtrees (blocks or function bodies) | renumbered by the new pre-order: each id of one subtree shifts by the size of the other subtree, so ids swap only when the two sizes are equal (`File=0, A=1, A.child=2, B=3` becomes `File=0, B=1, A=2, A.child=3`) | a `Block` has none; a body keeps its key, because each anchor stays with its declaration |
| Rename or re-nest the declaring container | can move | changes with the anchor (Stability against unrelated edits) |

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
| `--file cmd/main.go` | On Windows `\` becomes `/` (`os_path_to_slash`); on Unix `\` stays a file-name character. A leading `./` goes. A resolver keeps the best tier present: `Equal`, then `Suffix` (`/needle`), then `Substring`. |

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
| Bare name | A bare name matches top-level and scoped declarations. A top-level `helper` and a `helper` inside `outer` give `Ambiguous`; `::helper` and `outer.helper` each give `Unique` (`test_resolve_target_top_level_and_nested_helper_bare_name_ambiguous`). Exception: a bare name with no `--kind` and no `@line` drops each `Constructor` whose whole span, columns included, exactly one same-file type of the same name encloses, so `Foo` selects `class Foo` and not its constructors (`test_select_bare_type_two_overloads_both_drop`). An annotation type (C# attribute class, Kotlin `annotation class`) is a type here (`test_select_bare_annotation_type_drops_own_constructor`). The stored owner of a constructor is the bare class name, so the enclosing span is the proof. A constructor outside every same-named type's span (C++ `namespace B { Foo::Foo() {} }` below another `Foo`, `test_select_bare_out_of_class_constructor_stays`), including one beside the type on the same line (`test_select_bare_out_of_class_constructor_on_type_line_stays`), a constructor in another file, or an unrecorded owner keeps the ambiguity. `Foo.Foo`, `--kind constructor` and `@line` still reach the constructors. |
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
| same identity, other kind | builder pushes a tombstone (`builder.rs:1100-1118` comment, D1 collision recovery) | the tombstone has an empty name and no symbol-table entry | `UidSlot::Duplicate` for a kind that merges its repeats; each tombstone stores `uid::tombstone(uid::overload(base, ordinal))`, so the uids are pairwise distinct; every other kind is `Later` (Open point 4) |
| "keeps overloads" | `uid::keeps_overloads` (`uid.rs:33-35`): `Method`, `Constructor` | builder, view | used as is |
| overlay pairing | `dirty_base_by_uid` (`view.rs:348-355`), lookup at `view.rs:388`: by stored uid, so by position for overloads | `ViewNode.replaced_base` (`view.rs:254`) | `pair_by_anchor`; a `Shared` anchor pairs nothing |
| owner (M1, M2, M4) | parsers; `stamp_owner_class_by_span` (`ecp-analyzer/src/framework_helpers.rs:260`); Rust `enclosing_impl_type` (`rust/receiver_types.rs:286`), which strips the self type's generics; Rust `Impl` owner is the raw trait text, generics included (`rust/parser.rs:438-444`, `impl_trait_name` at `rust/receiver_types.rs:113-118`); Rust `Typedef` in an impl takes the same trait text (`rust/parser.rs:446-453`, `enclosing_impl_or_trait_context` at `rust/receiver_types.rs:258-273`) | `Node.owner_class` (`graph.rs:698-702`) | `DeclKey.owner`, text unchanged |
| owner spelling | `PathLiteral` owner is the enclosing owner verbatim (`post_process/path_literal_nodes.rs:45-70`), so `Walk<'s, 't>` sits beside the function owner `Walk` (review page, postparse F1); C++ `Foo<int>::m` takes owner `Foo` (`cpp/parser.rs:526-534`, `scope_type_name` at `cpp/receiver_types.rs:207-212`) | target lookup | identity keeps each text; the resolver compares by `owner_match_form` |
| Rust trait of an impl member | Method owner is the self type only (`enclosing_impl_type`); the `Impl` node's owner holds the trait | none for a member | the member's container is its `Impl` block, whose uid hashes the trait; no discriminator |
| overload signature | none: `RawNode` has no parameter list (`ecp-core/src/analyzer/types.rs:411-444`) | — | no identity fact; see Open point 2 |
| YAML resource kind | not in the uid (review page: `Service` and `Deployment` share one identity) | — | no identity fact; see Open point 2 |
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
| Rust global candidate filter | `rust_free_global_candidate` in `lookup_global` (`resolution/index.rs:1059-1081`) and the shared nearest-copy walk `nearest_copy` (`ecp-core/src/analyzer/ladder.rs`) | global tier | "scoped nodes are not global candidates" (design A I5), read from the parent link |

### Target request and ambiguity

| Fact | Produced at | Read at | Maps to |
|---|---|---|---|
| FQN split | `split_fqn_target`, last `.` (`ecp-cli/src/commands/symbol_id.rs:50-55`) | `impact` (`commands/impact/symbol.rs:133`), `resolve_candidates` (`symbol_id.rs:91`), `inspect` (`commands/inspect.rs:687`), `pattern` (`commands/pattern.rs:283`), `find-schema-bindings` (`commands/find_schema_bindings.rs:193`) | `TargetRequest::parse`. `a::b::f` now parses; today it is one bare name |
| owner match | `owner_matches`: equality, plus a last-segment match for `.rs` files only (`symbol_id.rs:57-64`); `inspect` uses equality (`inspect.rs:632`); `pattern` uses equality and a bare name means "no owner" (`pattern.rs:274-295`) | — | `Qualifier::matches`: segment-bounded suffix, every language |
| bare name in `find` | no owner form at all (review page, cli F1: `ecp find Engine.load` returns `found:false`) | — | `Qualifier::Path` |
| file needle | `normalize_file_needle`, substring, then `narrow_to_path_suffix`: equality, then `/`-bounded suffix (`symbol_id.rs:193-221`) | `impact`, `path` | `normalize_file_needle`, `file_match`, best tier in `select` |
| later overload as target | skipped (`symbol_id.rs:113`) | — | a candidate like any other; an ambiguity separates it |
| ambiguity, `impact` | `matches.len() > 1` gives `EcpError::AmbiguousSymbol` (`commands/impact/symbol.rs:153-192`) | `peers plan` (`commands/peers_plan.rs:64`), telemetry (`telemetry_cli.rs:52`), batch (`impact/mod.rs:378`) | `Ambiguity::to_error`, equal fields |
| ambiguity, `inspect` | returns every match as a full block (`inspect.rs:806`); an `Impl` beside a primary type is dropped first (`inspect.rs:703-740`) | agent | `Ambiguous`; the `Impl` rule becomes a kind filter the command applies before `resolve_target` |
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
| 2 (owner split) | `DeclKey`, `Scope`, `assign_file`, `lexical_parents` | writing M3 strings into `owner_class`; tombstoning a repeated scoped node; hashing a parent's positional uid; printing an owner the target syntax cannot read back |
| 5 (read path) | `TargetRequest::parse`, `resolve_target`, `TargetResolution`, `Ambiguity::to_error` | seven resolvers; the Rust-only suffix match; `inspect` returning full blocks for an ambiguity |
| 8 (routes) | `RouteKey`, `join_route_path`, `HandlerRef`, `RouteBinding` | two `uid::compute(Route, …)` sites; one node per producer for one endpoint; a handler bound by first-name lookup; `inline_anonymous` for a named handler that did not bind |
| 9 (Cypher) | node identity is the merged node index: `DISTINCT` on a node keys on it (`ecp-core/src/cypher/executor.rs:462-468`, `value.rs:53-57`); the `uid` property is `UidSlot::uid` (`executor.rs:2816`) | deduplicating nodes by name; reading `ownerClass` (`executor.rs:2817`) as an identity |

### Edge sources bind by row

A row is the index of a declaration in its file's raw records. Every emitter that starts or ends an edge at a declaration maps its row to a node through `RawIndexMap::node`. It never maps by `file_start + offset`. Two fallbacks map by name, and the rows below name them: the owner pass for a member that is not in a Rust `Impl` block, and an enum variant that no `Enum` span contains.

| Rule | Statement |
|---|---|
| Per-row emitters | Map the row, then dedupe. Never skip a merged row: it can carry a fact that its survivor lacks. `containment::scope_defines` serves the index and the overlay, and both dedupe `(source, child)` per file. A Python `x = 1; x = 2` gives exactly one `File -> x` on the full build and on the overlay. |
| Call source | A merged row's edge starts at its survivor, never at a nameless node. An edge equal to one the survivor already emits is dropped. |
| Class membership, span pass | The class is the node of the class row, and the member is the node of the member row. A `class C` twice (`a` in the first, `b` in the second) gives `HasMethod C@1 -> a` and `C@2 -> b`, and nothing else. Two merged `x` rows give one `HasProperty`. |
| Class membership, owner pass | A member of a Rust `Impl` block binds, in order, (1) the first type row of the owner's name whose container equals the `Impl`'s container, then (2) the `Impl` block itself. It never binds "the only type of that name in the file": that type can be the wrong one. Import evidence is Open point 15. Any other member that no span of its owner's name contains (a Go receiver, an out-of-line member) binds the file's first declaration of the owner's name (`lookup_in_file`). |
| Enum containment | The enum is the innermost `Enum` row whose span contains the variant. Both ends map through `node(raw)`. A variant that no `Enum` span contains keeps the name lookup. So `mod a { enum E { X } }` beside `mod b { enum E { X } }` gives two `E`, two `X`, and `Defines E@a -> X@a`, `E@b -> X@b`. |
| Overlay | The overlay uses the same rules through `FileMembers` and `survivor_of`. |

### Cross-area conflicts that name C1

| Pair | Requirement | How C1 meets it |
|---|---|---|
| 1, 2 | the global candidate filter (`resolution/index.rs:1073`, now `:1059-1081`), the lexical input, the uid pairing (`view.rs:388`) | one `lexical_parents` gives both the filter fact and the lexical input; `pair_by_anchor` replaces uid pairing. The order of the slices stays as the plan's Order section states. |
| 2, 5 | the target resolver reads the owner (`symbol_id.rs:141`) | the resolver reads the qualified path (owner plus parents), never the raw owner. A bare name matches top-level and scoped declarations; a tie is an ambiguity. The top-level meaning has its own syntax, `::name` (Open point 5, adopted). |
| 2, 8 | Route uids made in two places (`builder.rs:1411`, `:1481`) | `RouteKey::uid` is the one constructor. It hashes no handler and goes through no `assign_file` numbering, so a uid change of area 2 does not move a Route uid. |
| 2, 10 | the declaration anchor of a scope occurrence and `lexical_parents` both come from parser spans | "Scope occurrence identity": an occurrence takes the anchor and never changes it, and its Lexical parent row requires agreement, which a test checks. |

## Stored data

| Change | `GRAPH_FORMAT_VERSION` (20, `graph.rs:47`) | `BUILDER_FINGERPRINT` (`schema52`, `registry/commit_meta.rs:190`) | L1 fragment (`FRAGMENT_FORMAT_V7`, `session/overlay.rs:87`) | dirty manifest | view cache key (`VIEW_CACHE_FORMAT` 15, `session/view.rs:1401`) |
|---|---|---|---|---|---|
| adopt `DeclKey` for unscoped nodes, owner text as produced | none | none | none | none | none |
| `assign_file` in the four paths that skip ordinals (A-A) | none | bump | none | none | follows |
| the parent link table (A-B) | bump | bump | none | none | follows |
| a language opts in to scoped uids (A-C, A-D, A-E): every scoped node of that language changes its stored uid | none | bump | none: `FragmentSymbol` has kind, span and owner (`ecp-cli/src/session/overlay_writer.rs:85-96`) | none | follows |
| a repeated scoped node, and a `#[cfg]` twin under Open point 4, gets the overload uid `uid::overload(base, ordinal)`, not a tombstone | none | bump | none | none | follows |
| a producer changes its owner text (for example drops the generic arguments of the `PathLiteral` owner or of the Rust `Impl` and `Typedef` owners): the unscoped uids of those kinds change. C1 does not require it, because the resolver matches by `owner_match_form` | none | bump | none | none | follows |
| phase 1 (now removed): the anchor of a Rust trait-impl member hashed the trait | none | bump | none: the `Impl` nodes are in the fragment | none | follows |
| the container chain (phase 2): every declaration inside a scope container takes `nested_uid` from its container's anchor, and every node below it moves; the trait-hashing anchor of phase 1 is removed | none | bump (`+schema52`) | none: `FragmentSymbol` has kind, span and owner, so the overlay re-derives the containers | none | follows |
| a future `Signature` or `Tag` fact (Open points 1 and 2): a new stored table `discriminators: Vec<DiscriminatorFact { node_idx: u32, tag: u8, text: StrRef }>` in `ZeroCopyGraph`, appended at the end of the archived struct, and a new anchor stream | bump | bump (new parser fact; also changes scoped child uids when the language already opts in) | bump: `FragmentSymbol` lacks the fact | none | follows |
| `pair_by_anchor` in the overlay | none | none | none | none | follows: the binary path, size and mtime are in the key (`ecp-cli/src/session/view_cache.rs:36-43`) |
| `RouteKey` with composed prefixes; one node per key | none | bump | none | none | follows |
| `TargetRequest` / `resolve_target` | none | none | none | none | none |
| the scope table and import bindings (C4 "Scope table and import bindings"): no uid moves | bump: `ImportFact` gains `scope`, `site` and `form`, and the scope tables are stored | bump | bump: a new value after V7 | none | follows |

Not every stored uid stays as today. Four changes move uids, and each one bumps `BUILDER_FINGERPRINT` in the slice that makes it: a scoped node takes a uid from its parent's anchor; a member of a scope container takes a uid from its container's anchor; a repeated node takes the overload uid; a producer that changes its owner text changes the unscoped uids of its kinds. Only a top-level declaration whose owner text and kind do not change keeps today's uid.

The container of a base node comes from the stored kinds and spans of its file at read time (`path_identities`), so it needs no stored column. A future `Signature` or `Tag` fact has no node to read it from, so it needs the `discriminators` table above.

## Open points

| # | Decision | Options | Recommendation |
|---|---|---|---|
| 1 | Would a future discriminator enter the stored uid? | (a) anchor only; (b) the uid too | (a). The anchor equals the base uid today, so (a) needs a new anchor stream and a `BUILDER_FINGERPRINT` bump. The container chain already separates two trait-impl members, because each member hashes the anchor of its own `Impl` block, whose uid holds the trait: see the container-chain row of Stored data. |
| 2 | Who supplies a `Signature` or `Tag` fact? | (a) a parser field per language; (b) none, and `Shared` stays | (b) for now. Java, Kotlin, C#, C++, Swift and Dart overloads stay `Shared`, so they pair nothing and need `@line` as a target. Two overloads of one owner on one line cannot be told apart by `@line` either; they get no reusable target (`test_resolve_target_overloads_on_one_line_no_reusable`). Add (a) only when a measured case needs it. A future `Signature` or `Tag` fact re-adds an anchor stream (tag `0x02` is retired), needs the `discriminators` table and a `GRAPH_FORMAT_VERSION` bump, and needs a `BUILDER_FINGERPRINT` bump. |
| 3 | What pairs a `Shared` group in the overlay? | (a) nothing; (b) equal `content_hash` first, then the one remaining pair | (a) in the first slice; the view suppresses the base members and the caveat names them. (b) needs a per-symbol hash in `FragmentSymbol` (L1 bump). `impact --baseline` already pairs closures by body hash (`impact/baseline.rs:408-444`), so (b) has a precedent. |
| 4 | A `Duplicate` of a kind that does not keep overloads (a `#[cfg]` twin) | (a) tombstone, as today; (b) `Later` for every kind | Adopted: (b), except for `Property`, `Variable` and `Const`, which merge (`uid::merges_repeats`). `is_later_overload` reads the `later_nodes` bitset instead of recomputing `compute`. The twin stays findable and takes the overload uid, a stored-uid change (`GRAPH_FORMAT_VERSION` 20). Phase 2 narrows a repeat to one container: the same name in two different containers (`O1.H` and `O2.H`, `a::E` and `b::E`, a local class in two methods) is two `First` nodes, not a repeat. A merge happens only inside one container anchor, so the members of two `impl S` blocks, which share an anchor, still merge. |
| 5 | Does a bare name match scoped nodes? | (a) yes, and a tie is an ambiguity; (b) no, as design A §5.5 proposes | Adopted: (a). One rule for every command. `impact helper` with a top-level and a nested `helper` refuses the ambiguity and lists `::helper` and `outer.helper` (`test_resolve_target_top_level_and_nested_helper_bare_name_ambiguous`). The constructor exception of the Bare name row keeps a bare type name on a class with constructors from being ambiguous. This departs from design A §5.5. The plan's conflict row 2-5 states it. |
| 6 | The scope of a route key | (a) the registering file, as today; (b) the service root | (a). It keeps today's uids. Two services in one monorepo stay apart when their route files differ. |
| 7 | Where does `ImplTrait` come from for a base node? | (a) the enclosing `Impl` node's owner, by span, at read time; (b) a stored column | Superseded by phase 2: no `ImplTrait` discriminator remains, and the container comes from the spans at read time. Before: (a). The text is the full trait text as `impl_trait_name` produces it, generic arguments included (`Conv<u8>`), so `impl Conv<u8> for A` and `impl Conv<u16> for A` get two anchors. `graph.bin` has no per-file node range, so the reader scans the file's nodes, as the overlay already does for a dirty file (`view.rs:348-355`). |
| 8 | EntryPoint identity | (a) keep `compute(EntryPoint, path, None, "tag:name")`; (b) hash the handler's stored uid | (b), in area 8. The tag stays part of the key: the key hashes the tag plus the handler's stored uid, so one handler with two entry-point tags keeps two EntryPoint nodes. Today two same-named handlers of one file share an EntryPoint uid outside the controller path (`builder.rs:2070-2073`). |
| 9 | `impact --baseline` and `peers` identity keys | (a) adopt the anchor; (b) leave them | (b) in phase 2. Neither area in this contract owns them. File a follow-up when area 5 lands. |
| 10 | C++ explicit specializations | `Foo<int>::m` and `Foo<char>::m` | No change. The verified producer is the C++ `Method` path: `scope_type_name` (`cpp/receiver_types.rs:207-212`) already gives owner `Foo` (`cpp/parser.rs:526-534`), so these methods are `Later` overloads today and stay so. A target needs `@line`. Other C++ kinds were not traced. |
| 11 | Cypher `ownerClass` of a scoped node | (a) stored owner (`null`); (b) the display path | (a), plus a separate parent property that area 9 names. A property that changes meaning per node kind misleads a query. |
| 12 | The `Impl`-beside-type rule of `inspect` (`inspect.rs:703-740`) | (a) keep it as a command filter before `resolve_target`; (b) drop it | (a). It is a display choice of one command, not identity. An explicit `--kind` wins over it: `inspect A --kind impl` keeps the `Impl` node, and the rule applies only when no `--kind` is given. |
| 13 | C4 Open point 5: a closure handler | (a) a closure node is a `Node` endpoint; (b) always an unbound inline handler | (a), with `HandlerRef::Closure { uid }`. C4 reads a `Node(i)` endpoint whose node name starts with `<anonymous:` as `Closure { uid }`, any other `Node(i)` as `Declaration { uid }`, and `Unresolved(Anonymous)` or `Unresolved(Inline(_))` as `Inline { line }`. `Closure` and `Inline` both print `inline_anonymous`, so `ecp routes` output keeps its values. The uid lets `impact` reach the calls inside the closure. |
| 14 | Parser node coverage for missing containers (phase 3) | (a) emit the node; (b) leave the collision | (a). A TS `namespace` or `module` gives a `Namespace` node, and its members take the namespace as owner. A JS class expression gives a `Class` node. Until then the TS and JS cells marked `open` in Collision shapes per language still collide: their members have owner `None`, so a member of the second namespace binds to the first. The change bumps `BUILDER_FINGERPRINT`; `PARSER_FINGERPRINT` changes by itself. |
| 15 | Rust impl receiver with no same-container type | (a) bind the `Impl` block (owner pass rule 2); (b) bind the type through the file's `Import` rows and the module tree | (a) now. A member of `impl S` gets `HasMethod` from its `Impl` block when no same-named type shares the block's container: an `S` in another file, or `mod tests { use super::*; impl S {..} }` for an `S` of the parent module. (b) would bind `S` itself; the `Import` rows are in the graph already. Measure the edges that move on a corpus before choosing (b). |
| 16 | Shared anchors in a dirty file | (a) pair nothing, as Open point 3(a); (b) pair by position when the base and dirty counts are equal | (a) now, and a same-file C# `partial` loses its `PartOf` in the overlay. (b) keeps every pairing and the `PartOf` of an unchanged file. Against (b): an edit that inserts one same-key declaration and deletes another keeps the count, so a positional pair is wrong without a signal. `content_hash` pairing (Open point 3(b)) is honest but needs a `FragmentSymbol` field. |
| 17 | Qualified targets from the container chain (phase 4) | (a) derive the chain at read time; (b) leave the short owner | (a), as a follow-up. `ecp inspect O2.H.run` then resolves, and its reusable target is `O2.H.run` instead of `H.run@3 --file`. The read path derives the chain for the candidate files only, by a span scan (Open point 7), so `graph.bin` gains no column. `diff` and `impact --baseline` then key on (path, anchor, ordinal among the rows of that anchor); when one anchor has a different count in the two versions, every row of it is `changed` with an `ambiguous_match` flag, and the match never picks one (Open point 9). |
