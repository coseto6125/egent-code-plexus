# ecp cypher

Execute arbitrary graph queries using a subset of the Cypher query language.

## Usage
```bash
ecp cypher "MATCH (a)-[r]->(b) RETURN a,b" [--repo <PATH>]
```

## Subset Support
- **Boolean WHERE**: `AND`, `OR`, `NOT`.
- **Comparisons**: `=`, `!=`, `<`, `<=`, `>`, `>=`.
- **String Ops**: `STARTS WITH`, `ENDS WITH`, `CONTAINS`, `=~`, `IN [...]`.
- **Null / existence**: `IS [NOT] NULL` (pair with `OPTIONAL MATCH ... WITH f, c WHERE c IS NULL`; a `WHERE` directly after `OPTIONAL MATCH` is refused), `[NOT] EXISTS((a)-[:Rel]->(b))` (multi-hop + `*min..max` supported; multi-hop needs a var bound by an outer MATCH).
- **Functions**: only `type`, `id`, `labels`, `count`, `sum`, `avg`, `min`, `max`, `collect`. Any other name (`size`, `toLower`, ...) is an error.
- **Clauses**: one `WITH` per query; no `MATCH` after `WITH`. `ORDER BY` must name a projected key (alias it in `RETURN`). Comma patterns `(a)-[:X]->(m), (b)-[:Y]->(m)` join on shared variables.
- **Pathing**: Variable-length paths `[:Rel*1..2]` need `RETURN DISTINCT` (or `count(DISTINCT x)`), no edge variable, and a minimum of 0 or 1. Reverse arrows `<-[r]-` work on fixed-length patterns.
- **Lists**: index or slice a list value: `labels(n)[0]`, `xs[-1]`, `xs[1..3]`.

## NodeKinds
`Function`, `Method`, `Class`, `Property`, `Constructor`, `Interface`, `Const`, `Variable`, `Import`, `Route`, `Process`, `File`, `Struct`, `Enum`, `Trait`, `Impl`, `Module`, `Namespace`, `Typedef`, `Macro`, `Annotation`, `SchemaField`, `EventTopic`, `TransactionScope`, `PathLiteral`.

Category labels expand in MATCH and WHERE: `:Callable` (Function|Method|Constructor), `:Type` (Class|Interface|Struct|Enum|Typedef|Trait), `:Data` (Property|Variable|Const|EnumVariant|SchemaField). Prefer `:Callable` over bare `:Function` — the latter skips methods.

## RelTypes
`Calls`, `Extends`, `Imports`, `Implements`, `HasMethod`, `HasProperty`, `Accesses`, `ReadsField`, `HandlesRoute`, `References`, `Defines`, `Fetches`, `MirrorsField`, `Publishes`, `Subscribes`, `EventTopicMirror`, `OpensTxScope`, `Overrides`, `UsesPathLiteral`, `PartOf`.

A C# `partial` type has one node per part. Every other part has a `PartOf` edge to the representative part. `Extends` and `Implements` end at the representative. `ecp inspect` on any part lists the members of every part.

## Common patterns

### Path-literal split-brain (find filenames written one way, read another)
```cypher
MATCH (n:PathLiteral) WHERE n.name =~ ".*meta\\.json" RETURN n.name, n.filePath
```

### Who reads / writes a specific file?
```cypher
MATCH (s)-[r:USES_PATH_LITERAL]->(n:PathLiteral)
WHERE n.name = "session_meta.json"
RETURN s.name, r.reason
```
The `r.reason` payload is `sink:read|confidence:high`, `sink:write|confidence:high`, `sink:join|confidence:medium`, `sink:free|confidence:high`, etc. — split readers from writers without re-parsing.

### Who reads a constant, or passes a function as a callback?
```cypher
MATCH (s)-[r]->(c {name: "MAX_RETRIES"})
WHERE r.reason IN ["value_read", "function_reference"]
RETURN s.name, s.filePath, r.reason, r.confidence
```
A bare-name read of a constant or variable is a `ReadsField` edge with reason `value_read`. A function named without a call (a callback argument, a method reference) is a `References` edge with reason `function_reference`. `ecp impact` on the target already counts these readers.

## Process Tracing (Leiden-community-aware execution paths)

Processes are detected via forward BFS through the call graph, grouped by Leiden community. Each `Process` node represents an entry→terminal path; `StepInProcess` edges link member functions into that trace with a 1-indexed `step:N` reason label. Use these patterns to analyze execution flows, blast radius, and architectural bottlenecks.

### 1. List all processes with member count
Enumerate all processes, sorted by trace length.
```cypher
MATCH (p:Process)-[:StepInProcess]->(member)
RETURN p.name, count(member) AS step_count, p.filePath
ORDER BY step_count DESC
```
**CLI equivalent**: `ecp processes` shows all processes with stats; `ecp processes trace <name>` shows the full member chain for one process.

### 2. Find a process by entry/terminal name substring
Locate processes whose entry or terminal function contains a given substring (e.g., "render", "fetch").
```cypher
MATCH (p:Process)
WHERE p.name CONTAINS "render"
RETURN p.name, p.filePath
```
**CLI equivalent**: `ecp processes | grep render` or `ecp processes trace render` (if exact match found).

### 3. Members of a known process (step order)
Retrieve all functions in a process trace, ordered by step index.
```cypher
MATCH (member)<-[step:StepInProcess]-(p:Process)
WHERE p.name = "Main → HTTPServer"
RETURN member.name, step.reason, member.filePath
ORDER BY step.reason
```
**CLI equivalent**: `ecp processes trace "Main → HTTPServer"` — returns the trace with step numbers inline.

### 4. Processes whose members span several files
Find processes with two members in different files. Process nodes carry no community property, so file is the available boundary signal.
```cypher
MATCH (a)<-[:StepInProcess]-(p:Process)-[:StepInProcess]->(b)
WHERE a.filePath <> b.filePath
RETURN DISTINCT p.name, p.filePath
```
**CLI equivalent**: No direct shortcut; `ecp processes` lacks a cross-file filter.

### 5. Upstream callers into a process entry
Trace who calls the entry function of a process (one hop back via `Calls`).
```cypher
MATCH (caller)-[:Calls]->(entry)<-[:StepInProcess]-(p:Process)
WHERE p.name = "Main → HTTPServer"
RETURN DISTINCT caller.name, caller.filePath
```
**CLI equivalent**: Combine `ecp processes trace` + `ecp impact <entry-fn>` to list callers; requires two queries.

### 6. Process density per file
Identify files that host the most process members — potential architectural hubs.
```cypher
MATCH (member)<-[:StepInProcess]-(p:Process)
RETURN member.filePath AS file, count(DISTINCT p) AS process_count
ORDER BY process_count DESC
LIMIT 10
```
**CLI equivalent**: `ecp processes` shows per-process stats; no aggregation by file in the CLI.

### 7. Long-tail processes (highest step count)
Find the longest execution traces — useful for refactor prioritization and complexity reduction.
```cypher
MATCH (p:Process)-[:StepInProcess]->(member)
WITH p, count(member) AS step_count
WHERE step_count >= 5
RETURN p.name, step_count, p.filePath
ORDER BY step_count DESC
```
**CLI equivalent**: `ecp processes` output (if sortable).

### 8. Co-occurring processes (shared members)
Find processes that share a member function — indicates tightly-coupled execution paths.
```cypher
MATCH (member)<-[:StepInProcess]-(p1:Process), (member)<-[:StepInProcess]-(p2:Process)
WHERE id(p1) <> id(p2)
RETURN p1.name, p2.name, member.name, member.filePath
ORDER BY p1.name, p2.name
```
**CLI equivalent**: No built-in co-occurrence query; requires custom Cypher or post-processing.
