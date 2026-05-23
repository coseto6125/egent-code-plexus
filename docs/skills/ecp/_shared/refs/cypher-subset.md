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

## NodeKind inventory (28 variants)

Structural / files:
`File`, `Import`, `Namespace`, `Module`, `Impl`

Callable:
`Function`, `Method`, `Constructor`, `Macro`

Types (different runtime semantics — pick the right one for your query):
`Class` (reference type with vtable), `Struct` (value type, no vtable),
`Interface` (Java/C# style), `Trait` (Rust/Scala — distinct dispatch),
`Enum`, `EnumVariant`, `Typedef`, `Annotation`

Data:
`Property`, `Variable`, `Const`, `SchemaField` (DB-backed; distinct from Property)

Framework / orchestration:
`Route`, `EventTopic`, `TransactionScope`, `Process`

Scoring / docs:
`EntryPoint`, `Document`, `Section`

Run `ecp schema node-kinds` for each variant's load-bearing distinction (e.g. why `Struct` is distinct from `Class`, why `Trait` is distinct from `Interface`).

## RelType inventory (19 variants)

Containment / definition:
`Defines`, `HasMethod`, `HasProperty`

Type hierarchy:
`Extends`, `Implements`, `Overrides`

Dispatch:
`Calls`, `Accesses`, `References`

Routing:
`HandlesRoute`, `Fetches`, `StepInProcess`

Imports / annotations:
`Imports`, `Decorates`

Event / transaction:
`Publishes`, `Subscribes`, `EventTopicMirror` (heuristic), `OpensTxScope`

Heuristic schema-bridge:
`MirrorsField` (heuristic, confidence < 0.7 — filter unless `--include-heuristic`)

Run `ecp schema reltypes` for each edge's LLM-utility category and heuristic flag.

## BlindSpots

If a call site cannot be statically resolved, `ecp` emits a `BlindSpot` record instead of guessing an edge. This prevents hallucinations in the graph.

`BlindSpotRecord` fields: `kind`, `file_path`, `start_row` / `start_col` / `end_row` / `end_col`, `hint`, **`is_test: bool`** (true when the record originates from a test file — verdict layer filters these out of prod-refactor warnings).

Run `ecp schema blindspots` for per-language emitter coverage + the full list of kinds (~31 across 14 languages).
