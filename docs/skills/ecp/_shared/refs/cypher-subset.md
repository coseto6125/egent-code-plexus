# Cypher Subset Reference

EgentCodePlexus supports a subset of the openCypher query language optimized for structural analysis.

## Grammar
```cypher
MATCH (a:Kind)-[r:Rel]->(b:Kind)
WHERE a.name = 'X' AND r.confidence > 0.8
RETURN a.name, b.filePath
```

## Conventions
- **Keep queries minimal**: For complex analysis, use `ecp find` and post-process the JSON output.
- **NodeKind is case-sensitive**: `Function`, `Method`, `Class`, etc.
- **RelType is CamelCase**: `Calls`, `Extends`, `HasMethod`.

## NodeKind inventory (29 variants)

Structural / files:
`File`, `Import`, `Namespace`, `Module`, `Impl`

Callable:
`Function`, `Method`, `Constructor`, `Macro`

Types (different runtime semantics — pick the right one for your query):
`Class` (reference type with vtable), `Struct` (value type, no vtable),
`Interface` (Java/C# style), `Trait` (Rust/Scala — distinct dispatch),
`Enum`, `EnumVariant`, `Typedef`, `Annotation`

Data:
`Property`, `Variable`, `Const`, `SchemaField` (DB-backed; distinct from Property), `PathLiteral` (filesystem path / config key string)

Framework / orchestration:
`Route`, `EventTopic`, `TransactionScope`, `Process`

Scoring / docs:
`EntryPoint`, `Document`, `Section`

Run `ecp schema node-kinds` for each variant's load-bearing distinction (e.g. why `Struct` is distinct from `Class`, why `Trait` is distinct from `Interface`).

## RelType inventory (20 variants)

Containment / definition:
`Defines`, `HasMethod`, `HasProperty`

Type hierarchy:
`Extends`, `Implements`, `Overrides`

Dispatch:
`Calls`, `Accesses`, `References`

Routing:
`HandlesRoute`, `Fetches`, `StepInProcess`

Imports / annotations / path-refs:
`Imports`, `Decorates`, `UsesPathLiteral`

Event / transaction:
`Publishes`, `Subscribes`, `EventTopicMirror` (heuristic), `OpensTxScope`

Heuristic schema-bridge:
`MirrorsField` (heuristic, confidence < 0.7 — filter unless `--include-heuristic`)

Run `ecp schema reltypes` for each edge's LLM-utility category and heuristic flag.

### Recently-added kinds (cheat sheet)

- **EnumVariant** — `MATCH (e:Enum {name:'Status'})-[:Defines]->(v:EnumVariant) RETURN v.name`. `v.owner_class` carries the enum name.
- **Annotation** + **Decorates** — `MATCH (c:Class)-[:Decorates]->(a:Annotation {name:'Injectable'}) RETURN c`. Resolves to the annotation class on hit; otherwise targets a synthetic Annotation node deduped per name (its `file_idx` is `SYNTHETIC_FILE_IDX`; consumers indexing `graph.files[...]` must guard via `Node::has_owning_file()`). `m.decorators` cypher property gives the raw string list.
- **TransactionScope** + **OpensTxScope** — `MATCH (f:Function)-[:OpensTxScope]->(s:TransactionScope) RETURN f.name`. `s.name` carries the framework label (`tx_scope:{fn_name}#{spring-transactional|django-atomic|dotnet-transactional|symfony-transactional}`).
- **Implements** — class→interface targets distinguished from class→class via target's NodeKind (Interface / Trait → Implements; else Extends).
- **Fetches** — in-graph client→handler: `MATCH (f:Function)-[:Fetches]->(r:Route) RETURN r.name`. Cross-repo misses NOT emitted (use `ecp contracts` for that).
- **File→Defines** — top-level containment: `MATCH (f:File)-[:Defines]->(s) WHERE f.filePath ENDS WITH 'lib.rs' RETURN s.name`. Does NOT duplicate `HasMethod` / `HasProperty` from Class members.

## BlindSpots

If a call site cannot be statically resolved, `ecp` emits a `BlindSpot` record instead of guessing an edge. This prevents hallucinations in the graph.

`BlindSpotRecord` fields: `kind`, `file_path`, `start_row` / `start_col` / `end_row` / `end_col`, `hint`, **`is_test: bool`** (true when the record originates from a test file — verdict layer filters these out of prod-refactor warnings, so legitimate test fixtures using `eval` / reflection / `dlsym` to exercise prod code don't surface as noise).

Run `ecp schema blindspots` for per-language emitter coverage + the full list of kinds (~31 across 14 languages).
