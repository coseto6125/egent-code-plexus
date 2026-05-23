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
- **Aggregations**: `COUNT(*)`.
- **Pathing**: Variable-length paths `[:Rel*1..2]`, reverse arrows `<-[r]-`.

## NodeKinds
`Function`, `Method`, `Class`, `Property`, `Constructor`, `Interface`, `Const`, `Variable`, `Import`, `Route`, `Process`, `File`, `Struct`, `Enum`, `Trait`, `Impl`, `Module`, `Namespace`, `Typedef`, `Macro`, `Annotation`, `SchemaField`, `EventTopic`, `TransactionScope`, `PathLiteral`.

## RelTypes
`Calls`, `Extends`, `Imports`, `Implements`, `HasMethod`, `HasProperty`, `Accesses`, `HandlesRoute`, `References`, `Defines`, `Fetches`, `MirrorsField`, `Publishes`, `Subscribes`, `EventTopicMirror`, `OpensTxScope`, `Overrides`, `UsesPathLiteral`.

## Common patterns

### Path-literal split-brain (find filenames written one way, read another)
```cypher
MATCH (n:PathLiteral) WHERE n.name =~ ".*meta\\.json" RETURN n.name, n.file_path
```

### Who reads / writes a specific file?
```cypher
MATCH (s)-[r:USES_PATH_LITERAL]->(n:PathLiteral)
WHERE n.name = "session_meta.json"
RETURN s.name, r.reason
```
The `r.reason` payload is `sink:read|confidence:high`, `sink:write|confidence:high`, `sink:join|confidence:medium`, `sink:free|confidence:high`, etc. — split readers from writers without re-parsing.

## Process Tracing (Leiden-community-aware execution paths)

Processes are detected via forward BFS through the call graph, grouped by Leiden community. Each `Process` node represents an entry→terminal path; `StepInProcess` edges link member functions into that trace with a 1-indexed `step:N` reason label. Use these patterns to analyze execution flows, blast radius, and architectural bottlenecks.

### 1. List all processes with member count
Enumerate all processes, sorted by trace length.
```cypher
MATCH (p:Process)
WITH p, size([(member)<-[:StepInProcess]-(p) | member]) AS step_count
RETURN p.name, step_count, p.file_path
ORDER BY step_count DESC
```
**CLI equivalent**: `ecp processes` shows all processes with stats; `ecp processes trace <name>` shows the full member chain for one process.

### 2. Find a process by entry/terminal name substring
Locate processes whose entry or terminal function contains a given substring (e.g., "render", "fetch").
```cypher
MATCH (p:Process)
WHERE p.name CONTAINS "render"
RETURN p.name, p.file_path, p.community_id
```
**CLI equivalent**: `ecp processes | grep render` or `ecp processes trace render` (if exact match found).

### 3. Members of a known process (step order)
Retrieve all functions in a process trace, ordered by step index.
```cypher
MATCH (member)<-[step:StepInProcess]-(p:Process)
WHERE p.name = "Main → HTTPServer"
RETURN member.name, step.reason, member.file_path
ORDER BY step.reason
```
**CLI equivalent**: `ecp processes trace "Main → HTTPServer"` — returns the trace with step numbers inline.

### 4. Cross-community processes (members span multiple Leiden groups)
Find processes whose member functions belong to different communities — useful for identifying architectural boundaries.
```cypher
MATCH (member)<-[:StepInProcess]-(p:Process)
WITH p, collect(DISTINCT member.community_id) AS communities
WHERE size(communities) > 1
RETURN p.name, size(communities) AS distinct_communities, p.file_path
ORDER BY distinct_communities DESC
```
**CLI equivalent**: No direct shortcut; `ecp processes` lacks a `--cross-community` filter.

### 5. Upstream callers into a process entry
Trace who calls the entry function of a process (one hop back via `Calls`).
```cypher
MATCH (caller)-[:Calls]->(entry)<-[:StepInProcess]-(p:Process)
WHERE p.name = "Main → HTTPServer"
RETURN DISTINCT caller.name, caller.file_path
```
**CLI equivalent**: Combine `ecp processes trace` + `ecp impact <entry-fn>` to list callers; requires two queries.

### 6. Process density per file
Identify files that host the most process members — potential architectural hubs.
```cypher
MATCH (member)<-[:StepInProcess]-(p:Process)
WITH member.file_path AS file, count(DISTINCT p) AS process_count
RETURN file, process_count
ORDER BY process_count DESC
LIMIT 10
```
**CLI equivalent**: `ecp processes` shows per-process stats; no aggregation by file in the CLI.

### 7. Long-tail processes (highest step count)
Find the longest execution traces — useful for refactor prioritization and complexity reduction.
```cypher
MATCH (p:Process)
WITH p, size([(member)<-[:StepInProcess]-(p) | member]) AS step_count
WHERE step_count >= 5
RETURN p.name, step_count, p.file_path
ORDER BY step_count DESC
```
**CLI equivalent**: `ecp processes` output (if sortable).

### 8. Co-occurring processes (shared members)
Find processes that share a member function — indicates tightly-coupled execution paths.
```cypher
MATCH (member)<-[:StepInProcess]-(p1:Process), (member)<-[:StepInProcess]-(p2:Process)
WHERE p1 <> p2
RETURN p1.name, p2.name, member.name, member.file_path
ORDER BY p1.name, p2.name
```
**CLI equivalent**: No built-in co-occurrence query; requires custom Cypher or post-processing.
