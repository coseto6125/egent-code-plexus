Symbol blast radius — affected callers + risk_level. For binding tier-degradation or resolver delta, use `cgn diff`

Usage: cgn impact [OPTIONS] [NAME]

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
      --format <FORMAT>
          Output format (mostly internal — agent doesn't set this)
      --graph <GRAPH>
          Path to the graph.bin file [default: .cgn/graph.bin]
  -h, --help
          Print help
