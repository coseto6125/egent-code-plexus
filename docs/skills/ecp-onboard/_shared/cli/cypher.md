Cypher query escape hatch

Usage: ecp cypher [OPTIONS] [QUERY]

Arguments:
  [QUERY]
          The Cypher query string. Supports a read-only subset of openCypher:
          
          - Multi-hop patterns: (a)-[:Calls]->(b)-[:Calls]->(c) - Variable-length:    (a)-[:Calls*1..3]->(b) - Label alternation:  (a:Function|Method) - Category labels:    (a:Callable) = Function|Method|Constructor · :Type = Class|Interface|Struct|Enum|Typedef|Trait · :Data = Property|Variable|Const|EnumVariant|SchemaField - WHERE:              =, <>, <, <=, >, >=, AND, OR, NOT, IN, =~, CONTAINS, STARTS WITH, ENDS WITH - Properties (node):  name, kind, filePath, uid, ownerClass, lexicalParent, content, line, startLine, endLine, visibility, decorators, and the boolean flags is_test/is_async/is_static/is_abstract/is_generator/is_extern (camelCase aliases isTest/isAsync/… also accepted). An unknown property name fails the query with a structured error on stdout. `ownerClass` is the stored owner, null for a nested fn or closure: match a closure in `outer` with `n.lexicalParent = 'outer'` (the enclosing chain, such as `K::m`, else the stored owner). - Properties (edge):  confidence, reason, rel_type - Aggregation:        COUNT(*), COUNT(DISTINCT x), SUM/MIN/MAX/AVG, COLLECT - Pipeline:           one WITH ... [WHERE ...], OPTIONAL MATCH, UNION [ALL] (branches need equal column names: alias function columns with AS) - Output shaping:     RETURN [DISTINCT], ORDER BY, SKIP, LIMIT - Lists:              labels(n)[0], xs[-1], xs[1..3] (index or slice a list value)
          
          Evaluated with openCypher semantics (comma patterns join, EXISTS with both ends bound checks the pair, DISTINCT on a node counts nodes, `\d` reaches the regex engine, several MATCH WHEREs are ANDed). Refused with an error, never guessed: a second WITH, MATCH after WITH, WHERE right after OPTIONAL MATCH (use `WITH a, b WHERE ...`), ORDER BY outside the projection, functions other than type/id/labels and the aggregates, variable-length patterns without DISTINCT in RETURN, with an edge variable, or with a minimum above 1.
          
          Row budgets refuse a query that grows too large. Each hop, pattern and clause multiplies the rows before WHERE and LIMIT run. Patterns that share no variable, and hops through a node with many edges, multiply fastest. The opening scan and its first single hop count toward no budget. A plain LIMIT has no ORDER BY, DISTINCT or aggregate. A plain LIMIT whose SKIP plus LIMIT is at most 2,000,000 streams. It stops at its last row. It is refused past 20,000,000 visited rows. An aggregate streams too. It is refused past 20,000,000 visited rows. Every other query builds its rows. It is refused past 2,000,000 built rows. Patterns that share no variable are counted before the first row. The query is refused when their product passes its budget. A plain LIMIT skips this count. Past 64 nodes across its patterns, a query builds its rows. This includes aggregates and plain LIMITs.
          
          Cypher operates on a single graph; --repo must resolve to one repo.

Options:
      --query <QUERY>
          Named alias for the positional QUERY argument

      --repo <REPO>
          Repository to query. Cypher operates on a single graph (single-repo only). If --repo resolves to multiple repos, an error is returned

      --format <FORMAT>
          Output format. Absent or `llm`: the LLM-tuned toon payload. `toon`: uncompressed toon. `json`: full-fidelity JSON. `text`: pretty-printed JSON. An unaliased function column takes the function name in lower case and keeps its arguments, with `DISTINCT` in upper case: `count(b)`, `count(DISTINCT b)`, `labels(a)[0]`

          Possible values:
          - llm:  Default output: per-command LLM-tuned payload (token compression, rounded floats, trimmed timestamps, etc.) rendered through the toon encoder. Commands without command-specific tuning fall through to the same output as `Toon`
          - toon: Neutral toon: the json payload encoded as toon, no lossy compression. Use when you want round-trippable structure but tighter than JSON
          - json: Full-fidelity JSON. Always round-trippable; pick this when piping to another tool / asserting in tests
          - text

      --graph <GRAPH>
          Path to the graph.bin file
          
          [default: .ecp/graph.bin]

  -h, --help
          Print help (see a summary with '-h')
