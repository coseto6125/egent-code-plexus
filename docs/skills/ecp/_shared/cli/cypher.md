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
`Function`, `Method`, `Class`, `Property`, `Constructor`, `Interface`, `Const`, `Variable`, `Import`, `Route`, `Process`, `File`.

## RelTypes
`Calls`, `Extends`, `Imports`, `Implements`, `HasMethod`, `HasProperty`, `Accesses`, `HandlesRoute`, `References`, `Defines`, `Fetches`.
