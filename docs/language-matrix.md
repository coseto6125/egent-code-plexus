# Language Matrix

Per-language capability inventory across the 31 supported languages. Each cell answers one question: *for this language, do we extract this dimension yet?*

This matrix is **not** a parity scorecard against any other tool. We took design inspiration from GitNexus's 9-dimension breakdown (credit in [vs-gitnexus.md](./vs-gitnexus.md)) but every cell describes the state of *our* implementation.

## Legend

- **✓** &nbsp;implemented — we extract this for this language today
- **—** &nbsp;concept exists in the language but we don't extract it yet (concrete gap)
- **n/a** &nbsp;language linguistically lacks this concept (e.g. Bash has no class system, so Heritage / Ctor / Types are n/a)

The matrix is fully resolved — no "feasible but unimplemented" cells. Every `—` is a concrete TODO; every `n/a` is a non-target.

## Matrix

| Language | Imports | Named | Exports | Heritage | Types | Ctor | Config | Frameworks | Entry | Call | Rename | Group extractor |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| TypeScript | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ (HTTP + gRPC) |
| JavaScript | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ (HTTP + gRPC) |
| Python | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ (HTTP + gRPC) |
| Java | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ (HTTP + gRPC) |
| Kotlin | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | —[^ge] |
| C# | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | —[^ge] |
| Go | ✓ | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ (HTTP + gRPC) |
| Rust | ✓ | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ (HTTP + gRPC) |
| PHP | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | —[^ge] |
| Ruby | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | — | ✓ | ✓ | —[^ge] |
| Swift | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | —[^ge] |
| C | ✓ | ✓ | ✓ | — | ✓ | — | ✓ | — | ✓ | ✓ | ✓ | —[^ge] |
| C++ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | —[^ge] |
| Dart | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | —[^ge] |
| ─── *structural-only rows below* ─── | | | | | | | | | | | | |
| Bash | ✓ | ✓ | n/a | n/a | n/a | n/a | n/a | — | — | ✓ | ✓ | — |
| Lua | ✓ | ✓ | ✓ | ✓ | n/a | — | n/a | — | — | ✓ | ✓ | — |
| Solidity | ✓ | ✓ | ✓ | ✓ | — | — | n/a | — | — | ✓ | ✓ | — |
| Crystal | ✓ | ✓ | ✓ | ✓ | — | — | n/a | — | — | ✓ | ✓ | — |
| Nim | ✓ | ✓ | ✓ | ✓ | — | — | n/a | — | — | ✓ | ✓ | — |
| Cairo | ✓ | ✓ | ✓ | — | — | — | n/a | — | — | ✓ | ✓ | — |
| Move | ✓ | ✓ | ✓ | n/a | — | n/a | n/a | — | — | ✓ | ✓ | — |
| Zig | ✓ | ✓ | ✓ | n/a | — | — | n/a | — | — | ✓ | ✓ | — |
| HCL | ✓ | ✓ | ✓ | n/a | — | n/a | ✓ | — | — | ✓ | ✓ | — |
| SQL | n/a | ✓ | n/a | ✓ | — | n/a | n/a | n/a | n/a | ✓ | ✓ | — |
| Verilog | ✓ | ✓ | ✓ | — | — | — | n/a | — | — | ✓ | ✓ | — |
| Vyper | ✓ | ✓ | ✓ | n/a | — | — | n/a | — | — | ✓ | ✓ | — |
| Markdown | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | — |
| GitHub Actions | ✓ | n/a | ✓ | n/a | n/a | n/a | ✓ | n/a | — | n/a | n/a | — |
| Docker Compose | — | n/a | n/a | n/a | n/a | n/a | ✓ | n/a | n/a | n/a | n/a | — |
| Dockerfile | ✓ | n/a | n/a | n/a | n/a | n/a | ✓ | n/a | — | n/a | n/a | — |
| YAML | n/a | n/a | n/a | n/a | n/a | n/a | ✓ | n/a | n/a | n/a | n/a | — |

[^ge]: Extractor stub only — first-wave group extractor coverage limited to Go / Python / JS / TS / Java / Rust.

## Per-cell rationale

**Imports**

- *Bash:* `source` / `.`
- *Lua:* `require` + binding alias
- *Dockerfile:* `FROM <base>`
- *GitHub Actions:* `uses:` directives — public tag/SHA refs, local composites, reusable workflows, cross-repo workflows

**Named**

- *Bash:* `alias` command
- *Lua:* `local M = require(...)` and dotted-path bindings (plain literal RHS filtered)
- *Cairo:* `use X as Y` + `type X = Y`
- *Move:* `use ... as` alias clause (module + braced-member forms)
- *Zig:* `const X = @import(...)` / `const X = Identifier` (numeric/string/bool literal RHS filtered via parser-side priority promotion)
- *Crystal:* `alias X = Y`
- *Nim:* `type X = Y` with object/distinct/ref-type/tuple-object shapes filtered out (those stay Class)
- *Vyper:* `from X import Y as Z` / `import X as Y` (source-line scan — grammar can't AST-parse the `as` clause)
- *Solidity:* `using L for T` directives + `type C is uint256` user-defined value types
- *HCL:* `locals { }` block attributes (`output` blocks remain Const)
- *SQL:* top-level `CREATE VIEW v AS …` (column aliases `SELECT x AS y` not captured)
- *Verilog:* SystemVerilog `typedef` declarations
- *C:* `typedef` + `#define` / `preproc_function_def` + `extern` declarations (include-guard macros filtered; classified as Alias/Constant/Macro/Flag)
- *Swift:* `typealias` declarations + `@objc(extName)` rename attributes
- *Ruby:* `alias` keyword + `alias_method` + constant assignment (`MyConst = Other::Constant`) + `def_delegator` / `def_delegators` / `delegate` (with Forwardable mixin detection; cross-file `include Foo` propagation resolved via resolver Tier 2.75 HeritageScoped)
- *GitHub Actions / Docker Compose / Dockerfile* show `n/a` because these YAML/Dockerfile formats use keyed top-level entries (services, jobs, `ARG` / `LABEL`) — those are configuration keys already captured by the Config column, not re-bindable alias declarations.

**Exports**

- *Lua:* `function foo()` (top-level non-`local`)
- *Crystal:* default-public minus `private` / `protected` modifier
- *Nim:* trailing `*` marker
- *Cairo / Zig / Move:* `pub` / `public` / `entry` keyword
- *HCL:* `output` block
- *Vyper:* `@external` / `@view` / `@payable` decorators
- *Verilog:* SystemVerilog `class_property` minus `local` / `protected` qualifier
- *GitHub Actions:* `jobs.*.outputs` + `on.workflow_call.outputs`

**Heritage**

- *Lua:* `setmetatable(..., {__index=Parent})` heuristic
- *Solidity:* `is X, Y, Z`
- *SQL:* FK `REFERENCES` clauses — inline column-level, table-level, and named-constraint forms

**Ctor `—` on Go and Rust**

Neither language has a language-level constructor. Go uses factory functions (`NewFoo()`) and Rust uses associated functions (`Foo::new()`) as idiomatic substitutes, but the cross-language Ctor extractor only emits `NodeKind::Constructor` for languages with a reserved ctor name (`__init__`, `initialize`, `__construct`, `constructor`, `Class::Class`).

**Entry `—` on JavaScript and Ruby**

Absence of a language-level `main` convention (per `entry_points.rs` coverage table). Entry points still surface for these languages via route handlers and framework decorators — just not via a `main()` symbol.

**Rename `n/a` on markup/config rows**

Markdown, GitHub Actions, Docker Compose, Dockerfile, and YAML carry keys / literal strings, not re-bindable code identifiers — `ecp rename` would have nothing to rewrite.

## Schema emission coverage

The schema additions landed since 2026-05-22 (`Implements`, `EnumVariant`,
`Decorates`, `TransactionScope` + `OpensTxScope`, `PathLiteral` +
`UsesPathLiteral`, `Fetches`) emit on **different language subsets** —
this table is the ground truth so LLM context-builders know whether an
empty traversal means "no edges of this kind exist" or "ecp doesn't
emit this edge for this language yet". The runtime equivalent is
`ecp schema reltypes` (text or JSON) for the LLM-utility tier per edge,
plus `ecp schema blindspots` for per-language indirect-dispatch detection.

The 14 mainstream rows below match the ones above. Edge IDs map to
`RelType` variants in `crates/ecp-core/src/graph.rs`.

| Language | Implements | EnumVariant | Decorates | TransactionScope[^tx] | PathLiteral | Fetches |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: |
| TypeScript | ✓ | ✓ | ✓ | —[^typeorm] | ✓ | ✓ |
| JavaScript | ✓ | n/a[^js-enum] | ✓ | n/a | ✓ | ✓ |
| Python | ✓ | partial[^py-enum] | ✓ | ✓ (Django) | ✓ | ✓ |
| Java | ✓ | ✓ | ✓ | ✓ (Spring) | ✓ | ✓ |
| Kotlin | ✓[^kt-fix] | ✓ | ✓ | ✓ (Spring) | ✓ | ✓ |
| C# | ✓ | ✓ | ✓ | ✓ (.NET) | ✓ | ✓ |
| Go | ✓ | n/a[^go-enum] | ✓[^go-dec] | — | ✓ | ✓ |
| Rust | ✓ | ✓ | ✓ | — | ✓ | ✓ |
| PHP | ✓ | partial[^php-enum] | ✓ | ✓ (Symfony) | ✓ | ✓ |
| Ruby | ✓ | n/a[^rb-enum] | —[^rb-dec] | — | ✓ | ✓ |
| Swift | ✓ | ✓ | ✓ | —[^sw-tx] | ✓ | ✓ |
| C | ✓ | n/a[^c-enum] | ✓[^c-dec] | — | ✓ | ✓ |
| C++ | ✓ | ✓ | ✓[^cpp-dec] | — | ✓ | ✓ |
| Dart | ✓ | ✓ | ✓ | — | ✓ | ✓ |

Tracked extensions:

- **EnumVariant** — Python / PHP base-class detection + `enum X` for PHP 8.1+: FU-2026-05-23-011
- **Decorates** — full coverage across the 14 mainstream rows except Ruby (no annotation system); see per-language footnotes. FU-2026-05-23-012 closed.
- **TransactionScope** — TS/TypeORM, Rust `#[transaction]`, Dart/Go/Ruby/Swift annotation or call-site detectors: FU-2026-05-23-009; SQL-block form (Kotlin Exposed `transaction { … }`, Ruby `Model.transaction do … end`, raw `BEGIN; … COMMIT;`): FU-2026-05-23-018

Edges emitted on **every** indexed language (no per-language variance,
listed here for completeness so the matrix isn't read as exhaustive):

- `Defines` — File / Namespace / Module → contained symbol (Namespace + Module branch wired post-PR #372 / FU-2026-05-23-016)
- `Imports` — File → imported module/symbol
- `Calls` — caller → callee (carries `CallMeta` flags for indirect dispatch)
- `Extends` — subclass / subtrait → base type
- `HasMethod` / `HasProperty` — class container → member
- `Accesses` — reader/writer → variable / property
- `References` — generic reference fallback
- `Overrides` — method-level override (Java `@Override`, Kotlin `override`, C# `override`, C++ virtual-match)
- `HandlesRoute` — Function/Method → Route (varies by framework, not language)
- `Publishes` / `Subscribes` — event-bus producers / consumers (varies by framework)
- `MirrorsField` / `EventTopicMirror` — heuristic edges (confidence < 0.85; filtered by default)
- `StepInProcess` — emitted by the parser-agnostic `pass4_processes` post-process

[^tx]: TransactionScope / OpensTxScope cell shows the **framework** wired
on that language: a ✓ means the annotation-form detector exists, parens
name the framework. `—` means no annotation-form detector emitted yet;
the language may still use transactions via SQL-block form, tracked
under FU-2026-05-23-018.

[^typeorm]: TypeORM's `@Transactional` decorator is the missing piece for
TS — tracked in FU-2026-05-23-009 (T10 TransactionScope 5-langs entry).

[^js-enum]: JavaScript has no `enum` keyword; the OO-style "frozen
object literal" idiom isn't first-class enough to model as `EnumVariant`.

[^go-dec]: Go has no annotation syntax; `Decorates` is emitted from an
**allowlist** of symbol-level compiler pragmas — currently `noinline`,
`nosplit`, `noescape`, `linkname`, `norace`, `notinheap`, `nointerface`,
`nowritebarrier`, `nowritebarrierrec`, `yeswritebarrierrec`,
`registerparams`, `wasmimport`, `wasmexport`, `embed`. Excluded by design:
`//go:build` / `//go:binary-only-package` / `//go:debug` (file or package
scope) and `//go:generate` (build-pipeline directive, not symbol property).
Allowlist beats denylist so new package-scope directives don't silently
bleed into `Decorates`; see `crates/ecp-analyzer/src/go/parser.rs::GO_SYMBOL_PRAGMAS`.

[^rb-dec]: Ruby has no annotation system. The closest idioms (block-pass
`do …; end`, `prepend`/`include` mixins) carry different semantics and are
modelled by `Calls` / `Implements` respectively. No FU planned.

[^c-dec]: C `Decorates` covers C23 standard `[[attr]]` (`attribute_declaration`
node) and GNU `__attribute__((attr))` (`attribute_specifier` node). Both forms
attach as direct children of the declaration node; the parser walks them at
emit time. Useful subset: `[[nodiscard]]`, `[[deprecated]]`, `[[maybe_unused]]`,
`__attribute__((deprecated|pure|noreturn))`.

[^cpp-dec]: C++ `Decorates` shares the C path (`[[attr]]` /
`__attribute__((attr))`) and additionally preserves the `__override__`
sentinel that the existing `Overrides` post-process consumes — both flow
through the same `decorators` field, the `__override__` is filtered in
`decorates_edges` before edge emission.

[^py-enum]: Python uses `class Foo(Enum):` — first-class enum semantics require base-class detection (analogous to PR #356 Protocol detection). Tracked in FU-2026-05-23-011.

[^kt-fix]: Kotlin Implements was raw-fixture-only at PR #358; the parser path was fixed in PR #372 (`9d21a91 feat(kotlin)`) via the `is_interface_class()` demotion arm. FU-2026-05-23-017 done.

[^go-enum]: Go has no real `enum` — `const ( ... iota )` blocks model integer-typed constants but the discriminated-union semantics that make `EnumVariant` distinct from `Const` aren't present.

[^php-enum]: PHP 8.1+ has first-class `enum X { case A; }` syntax. Not yet captured; tracked in FU-2026-05-23-011.

[^rb-enum]: Ruby uses module constants for enum-like patterns; no `EnumVariant` semantics.

[^c-enum]: C `enum` constants are integer aliases — emitted as `Const`, not `EnumVariant`. C++ `enum class` is first-class and IS captured.

[^sw-tx]: Swift TransactionScope is wontfix in v1 (FU-2026-05-23-009, ✅ done
in PR #380 for the 5 sibling langs; Swift slot reserved by setup commit
`fb20e5dc`, no detector wired). Audit found no canonical pattern across
the Swift ecosystem:

- Core Data `context.performAndWait { … }` — lock-based thread-safety, not ACID. Excluded.
- GRDB `dbQueue.write { db in … }` — true transaction; needs `DatabaseQueue` receiver-type inference to avoid false positives.
- Realm `realm.write { … }` — true transaction; same receiver-type challenge.
- SQLite.swift `db.transaction { … }` — clearest name, but generic `obj.transaction { … }` fires on many non-DB receivers.

A robust detector requires import tracking (CoreData / GRDB / RealmSwift /
SQLite), receiver-type inference from variable assignments, per-framework
heuristics (`dbQueue` vs `realm` vs `db`), and a tree-sitter query
extension for trailing-closure call patterns — estimated 120-150 LOC
parser + 10-15 LOC query, with high false-positive risk without type
inference. Revisit when a specific framework (e.g., GRDB) has concrete
LLM demand; until then, zero scopes emitted is the correct outcome — no
framework found, not a missing detector.

## Call detection design

Call detection is centralised in `crates/ecp-analyzer/src/calls.rs`. The hot helper is `extract_calls(root, source, nodes, call_kinds)`:

- Each language parser passes the tree-sitter node kinds that represent a call in its grammar — e.g., `["call_expression"]` for JS/TS, `["function_call"]` for Lua, `["call"]` for Python.
- The walker is grammar-agnostic: descends the AST once, collects every call site, extracts the callee text via `callee_name_from(node, source)`, and attaches each call to its enclosing `Function` / `Method` via `attach_to_enclosing(line, callee, nodes)` (smallest-span containment).
- OO languages additionally bind a **receiver type** (`obj.method` → know what `obj` is). Each language has its own receiver-type module (`<lang>/receiver_types.rs`) tracking local variable annotations and class-scope `this` / `self`. The receiver type is stored on the RawCall so downstream resolution can pick the correct overload when method names collide.
- Reflection / dynamic dispatch (`getattr(self, name)()`, JS `obj[k]()`, etc.) is **not** speculatively resolved. It lands as a `BlindSpot` record (per the project's "honest unknown beats fabricated edge" principle — see [vs-gitnexus.md §3](./vs-gitnexus.md#3-honesty-about-unresolved-edges)).
- Call edges (`RelType::Calls`) are the largest single edge type in the graph; the saturating-conversion helper `safe_row` in `calls.rs` guards against rows exceeding `u32::MAX` corrupting call-to-function attribution.
