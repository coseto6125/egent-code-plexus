# Sub-projects 1/5 — Implements / Defines / Imports(Pass-2) / Fetches

Spec for lifting the **emit-zero invariant** at
`crates/ecp-analyzer/src/resolution/builder.rs:3017`. Four RelTypes are
pinned to zero emissions by a test assertion; this document defines the
emission semantics, resolution algorithm, and per-language behavior
required to lift each one.

This spec is **review-then-implement**: nothing is merged until the
emission rules below are agreed.

## 1. Why this exists

The graph schema (`crates/ecp-core/src/graph.rs`) reserves four
RelTypes that no parser currently emits:

- `RelType::Implements` — Class → Interface / Trait / Protocol
- `RelType::Defines`    — Scope container → Member (Class→Method, Namespace→Class, Module→Function …)
- `RelType::Imports`    — File → Imported Target *(emitted by `post_process/imports_edges.rs` already; Pass-2 path is the gap)*
- `RelType::Fetches`    — HTTP client call site → Route handler

A test at `builder.rs:3017` actively prevents emission of these four
from the Pass-2 hot path. Without them:

- "Which classes implement `Foo`?" — unanswerable via Cypher
- "All members of namespace `app.api`?" — unanswerable; partial via `HasMethod`+`HasProperty`
- Cross-repo client → route topology — only one direction (Route side) populated
- Pass-2 ↔ post-process parity — Imports is the lone RelType where Pass-2 lies (emits zero)

## 2. Out of scope

- **TransactionScope / OpensTxScope** — separate sub-project (#5)
- **EventTopic / Publishes / Subscribes** — already shipped, not part of this lift
- **MirrorsField / EventTopicMirror** — already shipped
- **Parsers' raw output schema** — `RawNode.heritage` stays a single
  `Vec<String>`; the Extends-vs-Implements split happens at edge-time,
  not parse-time (rationale in §4.2)

## 3. Current state

`builder.rs:3017` blocks these four. `Imports` has a workaround in
`crates/ecp-analyzer/src/post_process/imports_edges.rs` (Tier-1+2+3
resolution, 8 sub-steps). The other three are not emitted anywhere
in the analyzer.

The Pass-2 path that we are unblocking lives at
`pass2_emit_node_edges` (`builder.rs:1894`). It currently emits:

| RelType    | Source field             | Resolver target  | Reason interning   |
|------------|--------------------------|------------------|--------------------|
| Extends    | `raw_node.heritage`      | `Type`           | `reason_heritage`  |
| Calls      | `raw_node.calls`         | `Callable`       | `reason_call`      |
| Accesses   | `raw_node.type_annotation` | `Type`         | `reason_type`      |

The heritage walk emits **everything** in `heritage` as `Extends` (line
1917). Implements emission slots in at the same call site.

## 4. Implements — emission rules

### 4.1 Goal

Distinguish `Class -[:Extends]-> Class` (inheritance, single-parent in
most langs) from `Class -[:Implements]-> Interface` (multi-trait, no
state inheritance). Symmetrical for trait dispatch in Rust/Swift.

### 4.2 Why split at edge-time, not parse-time

Three options were considered:

| Option | Cost                                                          | Correctness                              |
|--------|---------------------------------------------------------------|------------------------------------------|
| (a) Split `heritage` into `extends`+`implements` in 14 parsers | High — 14 parsers, 14 test fixtures      | Authoritative per-language                |
| (b) Post-classify Extends edges by target node `kind`         | Low — single resolver pass             | Authoritative — `kind` is ground truth    |
| (c) Add parallel `implements: Vec<String>` field              | Medium — schema bump + 14 parsers        | Same as (a)                               |

**Pick (b).** The target's `NodeKind` (Interface / Trait / Protocol) is
the *definition* of the relationship — if the resolver lands on an
Interface node, the relationship IS Implements regardless of source
syntax (Java's `implements` keyword, Kotlin's `:` colon, Rust's
`impl X for Y`, Go's structural conformance — all converge on the same
graph shape).

(b) also handles a parser quirk for free: some languages don't
syntactically distinguish (Go has *no* `implements` keyword — its
interface conformance is implicit). The kind-based rule degrades
gracefully.

### 4.3 Algorithm

Inside `pass2_emit_node_edges` (`builder.rs:1906-1922`), at the loop
that walks `raw_node.heritage`:

```rust
for base in &raw_node.heritage {
    let targets = resolver.resolve_symbol(
        &local_graph.file_path,
        base,
        &local_graph.imports,
        ResolveTarget::Type,
    );
    for (target_id, confidence) in targets {
        let target_kind = symbol_table.node_kind(target_id);  // O(1) lookup
        let rel = match target_kind {
            NodeKind::Interface | NodeKind::Trait => RelType::Implements,
            _                                     => RelType::Extends,
        };
        edges.push(Edge {
            source: current_node_idx,
            target: target_id,
            rel_type: rel,
            confidence,
            reason: if rel == RelType::Implements { reason_implements } else { reason_heritage },
        });
    }
}
```

New StrRef constant `reason_implements = "pass2:implements"` interned
once during Pass-2 setup, mirroring `reason_heritage`.

### 4.4 Confidence

Same as current Extends — inherits from `resolver.resolve_symbol`.
No new heuristics; if the resolver is uncertain about the target,
that uncertainty propagates equally to whichever RelType we choose.

### 4.5 Languages — Interface / Trait / Protocol mapping

The kind-based dispatch only works if parsers emit the right NodeKind
for interface-like declarations. Audit table:

| Language    | Source construct                   | Current NodeKind | Action     |
|-------------|------------------------------------|------------------|------------|
| TypeScript  | `interface Foo { ... }`            | Interface        | OK         |
| Java        | `interface Foo { ... }`            | Interface        | OK         |
| Kotlin      | `interface Foo { ... }`            | Interface        | OK         |
| C#          | `interface IFoo { ... }`           | Interface        | OK         |
| Go          | `type Foo interface { ... }`       | Interface        | OK         |
| Rust        | `trait Foo { ... }`                | Trait            | OK         |
| Swift       | `protocol Foo { ... }`             | Interface        | **KEEP Interface** — Swift `protocol` cannot embed data, semantically closer to Java/C# interface than Rust trait |
| PHP         | `interface Foo { ... }`            | Interface        | OK         |
| Dart        | `abstract class Foo` (implicit)    | Class            | DEFER — Dart's implicit-interface convention is too noisy to elevate by default |
| Ruby        | Module mixin                       | Module           | DEFER — duck-typing has no first-class interface |
| Python      | `class Foo(Protocol):` (PEP-544)   | Class → **Interface** | **ADD DETECTION** — see §4.5b |
| C / C++     | (no interface concept; vtable)     | n/a              | DEFER      |
| JavaScript  | (no interface)                     | n/a              | DEFER      |

### 4.5b Python Protocol detection (NEW)

Python parser (`crates/ecp-analyzer/src/python/parser.rs`) shall promote
`NodeKind::Class` to `NodeKind::Interface` when the class's base list
**contains exclusively** Protocol-marker bases:

- `Protocol` (from `typing` or `typing_extensions`)
- `ABC` / `ABCMeta` (from `abc`)
- `Generic[...]` does NOT count (it's a parameterization marker, not an interface marker)

**Multiple-base corner case** (`class Foo(Bar, Protocol):`):

- If at least one base is **concrete** (i.e. not in the Protocol-marker set)
  → emit `NodeKind::Class` and record `is_protocol_like` flag in
  `FunctionMeta`-style sidecar (separate decision; tracked under #2 Annotation work).
- If **all** bases are Protocol-markers → emit `NodeKind::Interface`.

Rationale: a `class Foo(Bar, Protocol)` IS-A Bar (concrete inheritance)
AND structurally Protocol-like. Calling it Interface would mark the
`Foo → Bar` relationship as Implements, which is wrong (Bar is a class,
not an interface). Marking it Class preserves the relationship semantics;
the Protocol-ness can surface later via the Annotation work (#2).

Detection happens at parse time, before `RawNode.kind` is set. Affects
~30 LoC in `python/parser.rs` plus a fixture in
`crates/ecp-analyzer/tests/python_protocol_detection.rs`.

### 4.6 Test plan

Add `crates/ecp-analyzer/tests/implements_emission.rs` with fixtures
covering the 8 "OK" languages. Each fixture asserts:

- One Class → Interface Implements edge appears
- One Class → Class Extends edge appears in the same fixture
- Heritage chain `Class → Class → Interface` produces 1 Extends + 1 Implements (transitive Implements is NOT inferred — only direct)

## 5. Defines — emission rules

### 5.1 Goal — fill the scope-containment gap, do NOT duplicate

`HasMethod` and `HasProperty` already cover Class/Interface/Trait →
Method/Property containment. `Defines` exists to cover containment
relationships that those two RelTypes do **not** address:

- Namespace → Class (C#, PHP, C++)
- Namespace → Function (C#, PHP, C++ free functions)
- Module → Function / Class (Rust `mod foo { ... }`, Python `__init__.py`)
- File → top-level Function / Class / Const / Variable
- Trait → AssociatedType (Rust `type Item;` inside trait)

`Defines` does **NOT** emit for Class→Method, Class→Property,
Interface→Method, or Trait→Method — those remain HasMethod /
HasProperty exclusively.

### 5.2 Why not coexist with HasMethod (and emit both)

An earlier draft proposed parallel emission (both HasMethod and Defines
for the same Class→Method pair). Rejected because:

1. **Cognitive cost** — LLM querying class members has to pick between
   two RelTypes; picking the wrong one silently returns partial results.
   Violates the "explicit structural signal" principle: duplicated
   signals are noise, not redundancy.
2. **Edge-count cost** — ~2x edges for class members hurts graph.bin
   size and Pass-2 sort/serialize time. CLAUDE.md priority §3 (signal
   density) makes this a regression.
3. **Querying broader containment is cheap** —
   `MATCH (c)-[:HasMethod|HasProperty|Defines]->(m)` is one extra
   alternation in the LLM's mental model, no graph cost.

Decision tree for LLMs becomes "source kind → which RelType":

- Source is Class / Interface / Trait → `HasMethod` / `HasProperty`
- Source is Namespace / Module / File → `Defines`

No overlap, no choice paralysis.

### 5.3 Algorithm — single site, post-walk only

After Pass-2's per-file walk completes (no changes inside
`pass2_emit_node_edges`), run a new helper
`pass2_emit_scope_defines` for each `LocalGraph`:

```rust
fn pass2_emit_scope_defines(
    local_graph: &LocalGraph,
    symbol_table: &SymbolTable,
    file_node_idx: u32,
    reason_defines: StrRef,
    edges: &mut Vec<Edge>,
) {
    let file_path_lossy = local_graph.file_path.to_string_lossy().replace('\\', "/");

    for raw_node in &local_graph.nodes {
        // Skip nodes that already have a Class-side container — covered by HasMethod/HasProperty.
        if raw_node.owner_class.is_some() {
            continue;
        }

        let Some(child_id) = symbol_table.lookup_in_file(&file_path_lossy, &raw_node.name)
        else { continue };

        match raw_node.kind {
            // File → top-level symbol
            NodeKind::Function | NodeKind::Class | NodeKind::Interface
            | NodeKind::Trait    | NodeKind::Const | NodeKind::Variable
            | NodeKind::Struct   | NodeKind::Enum  | NodeKind::Typedef
            | NodeKind::Macro    | NodeKind::Module | NodeKind::Namespace => {
                edges.push(Edge {
                    source: file_node_idx,
                    target: child_id,
                    rel_type: RelType::Defines,
                    confidence: 1.0,
                    reason: reason_defines,
                });
            }
            _ => {}
        }
    }

    // Pass 2: Namespace / Module containers — same file, owner_class matches.
    for container in local_graph.nodes.iter().filter(|n|
        matches!(n.kind, NodeKind::Namespace | NodeKind::Module))
    {
        let Some(container_id) = symbol_table.lookup_in_file(&file_path_lossy, &container.name)
        else { continue };

        for child in local_graph.nodes.iter().filter(|n|
            n.owner_class.as_deref() == Some(&container.name))
        {
            let Some(child_id) = symbol_table.lookup_in_file(&file_path_lossy, &child.name)
            else { continue };
            edges.push(Edge {
                source: container_id,
                target: child_id,
                rel_type: RelType::Defines,
                confidence: 1.0,
                reason: reason_defines,
            });
        }
    }
}
```

Two-pass within the helper:
- Pass 1: File → top-level (`owner_class.is_none()`)
- Pass 2: Namespace/Module → child (`owner_class.as_deref() == Some(container.name)`)

Confidence is `1.0` — `owner_class` is parser-provided ground truth,
not resolver inference.

~80 LoC including the per-LocalGraph driver loop.

### 5.4 Reason interning

```rust
let reason_defines = string_pool.add("pass2:defines");
```

Single reason — no need to discriminate "class-defines-method" vs
"module-defines-function" at the reason level since the source and
target kinds already encode that.

### 5.5 Test plan

Add `crates/ecp-analyzer/tests/defines_emission.rs` covering:

- Namespace → Class (C# `namespace App.Api { class Foo {} }`)
- Namespace → Function (PHP `namespace App; function bar() {}`)
- Module → Function (Rust `mod foo { fn bar() {} }`)
- Module → Class (Python `app/__init__.py` exporting classes)
- File → top-level Function (Python module-level `def`)
- File → top-level Const (TS/JS `export const X = ...`)
- **No-duplication invariant**: Class → Method emits HasMethod ONLY
  (Defines must NOT appear); regression-test on Java/Python/Rust fixtures.
- **owner_class==None gate**: confirm methods with `owner_class.is_some()`
  are skipped in the File→top-level pass.

## 6. Imports (Pass-2 parity)

### 6.1 Status

`post_process/imports_edges.rs` already emits Imports edges with a
sophisticated 8-step resolver. **Pass-2 itself emits zero.**

The emit-zero assertion at `builder.rs:3017` checks the Pass-2
buckets specifically, NOT the final graph state. Post-process Imports
edges DO appear in `graph.bin`.

### 6.2 Recommendation: leave Imports in post-process

The 8-step resolver in `imports_edges.rs` has cross-file basename
indexes (`basename_idx`, `dir_component_idx`) that are built once
across the whole repo. Moving this to Pass-2 either:

- Duplicates the index per-Pass-2-batch (memory cost), OR
- Builds it before Pass-2 (breaks the streaming-friendly Pass-2 design)

**Action**: Update the assertion at `builder.rs:3017` to **remove
"Imports"** from the unimplemented list. The comment should change
from "Sub-projects 1/5 will lift this" to "Imports is intentionally
post-process; see imports_edges.rs §Tier-1-2-3."

### 6.3 Test plan

Add a unit test in `builder.rs::tests` asserting that Pass-2 does
NOT emit Imports (current behavior), and a docs link to the
post-process spec.

## 7. Fetches — emission rules

### 7.1 Goal

HTTP client call site → Route node, when both ends are in the same
indexed graph (single-repo or `@group`).

### 7.2 Source

Each parser's HTTP client detector (e.g. `framework_helpers::http_client`)
already collects per-call-site metadata: method, URL template,
caller span. This metadata currently feeds the cross-repo `contracts`
command via a side-channel (`RawFanoutRef` + `client_calls` aggregator).

The gap: the Calls edge is emitted (caller → fetch helper), but no
edge connects caller to the *matching Route handler* in the same graph.

### 7.3 Algorithm

After Pass-2's per-node walk completes, run a third pass:

1. Build `RouteIndex`: `(method, normalized_path)` → `Route node_idx`
   from all `RawRoute` records.
2. For each `RawFanoutRef` (or analogous client-call structure) with
   `framework_id == http_client` and parseable `(method, path_pattern)`:
   - Normalize path (strip leading `/`, lowercase host, collapse params)
   - Match against `RouteIndex`
   - On hit: emit `Edge { source: caller, target: route_node_idx, rel_type: Fetches, confidence: ~0.8, reason: "pass2:fetches" }`
   - On miss: do NOT emit (cross-repo case — let `contracts` handle it)

### 7.4 Confidence

`0.8` baseline. Higher if exact path match (no wildcards); lower
(0.6) if path contains template params that needed coalescing.

### 7.5 Out of scope for this lift

- Cross-repo Fetches (handled by `contracts` command, not graph schema)
- gRPC / GraphQL / message queue Fetches (separate spec, share the
  same edge type but different normalization rules)
- WebSocket / SSE long-lived connections

### 7.6 Test plan

Add `crates/ecp-analyzer/tests/fetches_emission.rs` covering:

- TS `fetch('/api/users')` → matching `app.get('/api/users', handler)` → 1 Fetches edge
- Python `requests.get('/api/users')` → matching Flask route → 1 Fetches edge
- Method mismatch (GET vs POST) → 0 edges
- Path with `:param` template → coalesces → emits with confidence 0.6

## 8. Migration order

Each numbered step is a separate PR. Each PR lifts ONE element of the
`unimplemented` array at `builder.rs:3017`. CI gates ensure prior PRs
stay green.

| PR | Lift                            | Estimated LoC | Risk |
|----|---------------------------------|---------------|------|
| #1 | Imports (assertion-only)        | ~20           | Low — no code change beyond test |
| #2 | Implements (kind-based dispatch) | ~120         | Medium — node_kind lookup on hot path |
| #3 | Python Protocol detection (§4.5b) | ~50         | Low — single-parser change |
| #4 | Defines (scope-containment fill) | ~180         | Medium — owner_class scan, no resolver |
| #5 | Fetches (RouteIndex)             | ~250         | Medium-high — RouteIndex building cost |

Total: **~620 LoC** core code + **~430 LoC** tests = ~1050 LoC. Down
from the earlier ~1200 estimate because Defines collapsed from 2 PRs
to 1 (no Class-side duplication).

## 9. Performance budget

Pass-2 currently runs ~25k files in <5s (per CLAUDE.md priority §1).
Each PR must stay within:

- **+5% Pass-2 wall-clock** for PRs #1-#4
- **+10% Pass-2 wall-clock** for PR #5 (RouteIndex build is genuine new work)

Benchmark before/after via `python scripts/benchmark/benchmark_ecp.py`,
include numbers in PR body.

## 10. Resolved decisions (locked)

User confirmed these design points during spec review (2026-05-23):

1. **Implements via kind-based dispatch** — §4.2 option (b). No
   `RawNode.heritage` split at the parser level. Dispatch happens at
   edge emit time, using `target.kind == Interface | Trait`.
2. **Swift `protocol` stays `NodeKind::Interface`** — closer to Java/C#
   semantics than Rust trait (cannot embed state).
3. **Python `Protocol` / `ABC` detection added** — §4.5b promotes
   `Class → Interface` when **all** bases are Protocol-markers; mixed
   bases (e.g. `class Foo(Bar, Protocol)`) stay `Class` to preserve
   inheritance semantics.
4. **Defines does NOT duplicate HasMethod / HasProperty** — §5.2.
   Defines fills only Namespace/Module/File scope containment;
   Class-side containment stays exclusively HasMethod / HasProperty.
5. **Imports remains in post-process** — §6.2. Pass-2 assertion drops
   the "Imports" entry; the comment links to `imports_edges.rs`.
6. **Fetches in-graph only** — §7.5. Cross-repo HTTP client traffic
   stays out of `graph.bin`; `ecp contracts` retains exclusive cross-
   repo responsibility.

## 11. Open question (1)

**PR sequencing** — ship as 5 separate small PRs per §8 (clearer review,
more CI runs) or bundle PRs #1-#4 (everything except Fetches) into one
larger PR + Fetches as a second (fewer CI runs, harder review)?

Recommendation: **5 separate PRs**. Each lift toggles one element of
the `unimplemented` array, which makes regression bisection trivial
if a downstream query starts producing wrong results.

## 12. Out-of-scope decisions documented

- **`RawNode.heritage` split** rejected — see §4.2 option (a) vs (b)
- **New `is_interface_like` flag** rejected — kind-based dispatch is sufficient for the 8 OK languages
- **Imports in Pass-2** rejected — see §6.2
- **Transitive Implements inference** rejected — only direct
  Implements relationships are emitted (transitivity is a query-time
  concern via `[:Implements|Extends*1..N]`)
- **Defines parallel-with-HasMethod** rejected — see §5.2
- **TransactionScope / OpensTxScope** out of this lift — sub-project
  #5, separate spec (annotation + SQL block detection)
