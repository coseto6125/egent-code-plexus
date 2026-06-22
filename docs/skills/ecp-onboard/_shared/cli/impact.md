Symbol blast radius — affected callers (counts + call sites). For binding tier-degradation or resolver delta, use `ecp diff`

Usage: ecp impact [OPTIONS] [NAME]

Arguments:
  [NAME]  Target symbol name (mutually exclusive with --baseline). Equivalent to the `--target` named form below

Options:
      --target <TARGET>
          Named alias for the positional NAME argument — kept for parity with old MCP / wrapper habits
      --baseline <BASELINE>
          Git ref — compute blast radius across all symbols changed between this baseline and HEAD. Mutually exclusive with positional <name>
      --file_path <FILE>
          Disambiguate when name has multiple matches: substring on file path
      --kind <KIND>
          Disambiguate by kind (function | method | class | route | ...)
      --direction <DIRECTION>
          Direction of traversal [default: up] [possible values: up, down, both]
      --depth <DEPTH>
          Maximum BFS depth [default: 5]
      --high-trust-only <HIGH_TRUST_ONLY>
          Default OFF — recall-first: traverse every edge regardless of confidence (cross-crate refs at 0.7 are still real callers, just less certain). Pass `--high-trust-only=true` to restrict to confidence ≥ 0.8 edges for a noise-light view; when filtering kicks in, the output reports `hidden_edges` so missed coverage stays visible [default: false] [possible values: true, false]
      --min-confidence <MIN_CONFIDENCE>
          Override the high-trust threshold with a custom value (0.0–1.0). If set, takes precedence over --high-trust-only
      --include-tests
          Include test files in traversal
      --relation_types <RELATION_TYPES>
          Comma-separated relation types to follow (calls, extends, ...)
      --repo <REPO>
          Repository selector
      --test-coverage
          Coverage gap analysis: for each touched symbol, classify by test-caller presence (uncovered / partial / covered). Uses FunctionMeta.is_test flag from per-language extraction. Outputs uncovered symbols first to support LLM PR review ("X 改了沒測試"). Implies --include-tests during traversal so test callers are reachable from the walker
      --include-heuristic
          Include heuristic edges (MirrorsField, EventTopicMirror) in BFS. Default off keeps blast-radius results noise-free
      --confidence-threshold <CONFIDENCE_THRESHOLD>
          Informational confidence gate — promotes heuristic edges when T4-7/T5-33 emit per-edge tiers. Currently controls the --explain-confidence report [default: 0.85]
      --explain-confidence
          Emit explain_confidence block with threshold + per-tier filtered counts
      --format <FORMAT>
          Output format (mostly internal — agent doesn't set this)
      --literal <VALUE>
          List sites of a path-shaped string literal by exact value. Mutually exclusive with --target/--baseline/<name>. Returns JSON with each site's file, line, enclosing fn, and sink classification (`sink:read` / `sink:write` / `sink:open-read` / `sink:join` / etc). Designed for LLM split-brain queries: `ecp impact --literal session_meta.json` answers "where is this file read or written?" without writing cypher
      --graph <GRAPH>
          Path to the graph.bin file [default: .ecp/graph.bin]
  -h, --help
          Print help
