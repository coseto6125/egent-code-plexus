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

## Full schema

**NodeKind** (28 kinds, case-sensitive):
`File / Function / Class / Method / Interface / Constructor / Property / Variable / Const / Import / Route / Process / Document / Section / EntryPoint / Struct / Enum / Typedef / Namespace / Module / Macro / Annotation / Trait / Impl / SchemaField / EventTopic / TransactionScope / EnumVariant`.

**RelType** (18 kinds, CamelCase):
`Defines / Imports / Calls / Extends / Implements / HasMethod / HasProperty / Accesses / HandlesRoute / StepInProcess / References / Fetches / MirrorsField / Publishes / Subscribes / EventTopicMirror / OpensTxScope / Overrides`.

### Recently-added kinds (cheat sheet)
- **EnumVariant** — `MATCH (e:Enum {name:'Status'})-[:Defines]->(v:EnumVariant) RETURN v.name`. `v.owner_class` carries the enum name.
- **Annotation** + **Decorates** — `MATCH (c:Class)-[:Decorates]->(a:Annotation {name:'Injectable'}) RETURN c`. Resolves to the annotation class on hit; otherwise targets a synthetic Annotation node deduped per name. `m.decorators` cypher property gives the raw string list.
- **TransactionScope** + **OpensTxScope** — `MATCH (f:Function)-[:OpensTxScope]->(s:TransactionScope) RETURN f.name`. `s.name` carries the framework label (`tx_scope:{fn_name}#{spring-transactional|django-atomic|dotnet-transactional|symfony-transactional}`).
- **Implements** — class→interface targets distinguished from class→class via target's NodeKind (Interface / Trait → Implements; else Extends).
- **Fetches** — in-graph client→handler: `MATCH (f:Function)-[:Fetches]->(r:Route) RETURN r.name`. Cross-repo misses NOT emitted (use `ecp contracts` for that).
- **File→Defines** — top-level containment: `MATCH (f:File)-[:Defines]->(s) WHERE f.filePath ENDS WITH 'lib.rs' RETURN s.name`. Does NOT duplicate `HasMethod` / `HasProperty` from Class members.

## BlindSpots
If a call site cannot be statically resolved, `ecp` emits a `BlindSpot` record instead of guessing an edge. This prevents hallucinations in the graph.
