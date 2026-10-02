Symbol blast radius — affected callers (counts + call sites).

For binding tier-degradation or resolver delta, use `ecp diff`.

Usage: ecp impact [OPTIONS] [NAME]

Arguments:
  [NAME]
          Target symbol name (mutually exclusive with --baseline). Equivalent to the `--target` named form below. Optional when `--batch` is set or when a non-positional mode flag (`--target`, `--baseline`, `--literal`, `--literal-coherence`) supplies the query

Options:
      --target <TARGET>
          Named alias for the positional NAME argument — kept for parity with old MCP / wrapper habits

      --baseline <BASELINE>
          Git ref — compute blast radius across all symbols changed between this baseline and HEAD. Mutually exclusive with positional <name>

      --file <FILE>
          Disambiguate when name has multiple matches: substring on file path. `--file_path` / `--file-path` stay as aliases for back-compat

      --kind <KIND>
          Disambiguate by kind (function | method | class | route | ...)

      --direction <DIRECTION>
          Direction of traversal
          
          [default: up]
          [possible values: up, down, both]

      --depth <DEPTH>
          Maximum BFS depth
          
          [default: 5]

      --high-trust-only <HIGH_TRUST_ONLY>
          Default OFF — recall-first: traverse every edge regardless of confidence (cross-crate refs at 0.7 are still real callers, just less certain). Pass `--high-trust-only=true` to restrict to confidence ≥ 0.8 edges for a noise-light view; when filtering kicks in, the output reports `hidden_edges` so missed coverage stays visible
          
          [default: false]
          [possible values: true, false]

      --min-confidence <MIN_CONFIDENCE>
          Override the high-trust threshold with a custom value (0.0–1.0). If set, takes precedence over --high-trust-only

      --include-tests
          Test-file callers are listed by default, tagged `test: true`, since a rename breaks them too. This flag is kept for old callers; on its own it changes one thing: `--baseline` also treats changed test files as changed symbols

      --exclude-tests
          Drop test-file callers; the payload reports `hidden_test_callers: N`

      --relation_types <RELATION_TYPES>
          Comma-separated relation types to follow (calls, extends, ...)

      --repo <REPO>
          Repository selector

      --test-coverage
          Coverage gap analysis: for each touched symbol, classify by test-caller presence (uncovered / partial / covered). Uses FunctionMeta.is_test flag from per-language extraction. Outputs uncovered symbols first to support LLM PR review ("X 改了沒測試"). Implies --include-tests during traversal so test callers are reachable from the walker

      --no-heuristic
          Suppress heuristic callers (MirrorsField, EventTopicMirror) from the blast radius. Default: heuristic callers ARE shown, in a separate `heuristic_callers` bucket tagged `requires_verification`. Pass this flag for a pure-deterministic blast radius

      --confidence-threshold <CONFIDENCE_THRESHOLD>
          Informational confidence gate — promotes heuristic edges when T4-7/T5-33 emit per-edge tiers. Currently controls the --explain-confidence report
          
          [default: 0.85]

      --explain-confidence
          Emit explain_confidence block with threshold + per-tier filtered counts

      --format <FORMAT>
          Output format (mostly internal — agent doesn't set this)

      --literal <VALUE>
          List sites of a path-shaped string literal by exact value. Mutually exclusive with --target/--baseline/<name>. Returns JSON with each site's file, line, enclosing fn, and sink classification (`sink:read` / `sink:write` / `sink:open-read` / `sink:join` / etc). Designed for LLM split-brain queries: `ecp impact --literal session_meta.json` answers "where is this file read or written?" without writing cypher

      --literal-coherence
          Auto-detect likely path-literal split-brain pairs across all PathLiteral nodes. Conservative: same extension, similar basename, nearby directories, and read-only vs write-only sink separation

      --batch
          Read target symbol names from stdin (one per line; `#` and blank lines skipped). The graph is loaded once and N symbols are resolved sequentially — amortises mmap + process spawn across queries. Each result is prefixed by `=== target: <name> ===` so callers can split the stream unambiguously. Flags like --direction / --depth / --include-tests apply uniformly to all targets.
          
          Symbol-mode only: `--batch` combined with `--baseline` or `--literal` is rejected as an invalid argument. A positional name is also rejected — stdin is the single source of targets, so a positional would be silently ignored otherwise.

      --graph <GRAPH>
          Path to the graph.bin file
          
          [default: .ecp/graph.bin]

  -h, --help
          Print help (see a summary with '-h')
