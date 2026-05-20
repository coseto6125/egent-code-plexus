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

## BlindSpots
If a call site cannot be statically resolved, `ecp` emits a `BlindSpot` record instead of guessing an edge. This prevents hallucinations in the graph.
