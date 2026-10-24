Show a symbol's full context: signature, body, edges, callers, overrides, 1-hop impact

Usage: ecp inspect [OPTIONS] [NAME]

Arguments:
  [NAME]
          Target symbol name (equivalent to `--name` flag)

Options:
      --name <NAME>
          Named alias for the positional NAME argument — kept for parity with old MCP / wrapper habits

      --repo <REPO>
          Repository path

      --format <FORMAT>
          Output format. Absent or `llm`: the LLM-tuned toon payload. `toon`: uncompressed toon. `json`: full-fidelity JSON. `text`: pretty-printed JSON. Every format except `json` drops the edge boilerplate

          Possible values:
          - llm:  Default output: per-command LLM-tuned payload (token compression, rounded floats, trimmed timestamps, etc.) rendered through the toon encoder. Commands without command-specific tuning fall through to the same output as `Toon`
          - toon: Neutral toon: the json payload encoded as toon, no lossy compression. Use when you want round-trippable structure but tighter than JSON
          - json: Full-fidelity JSON. Always round-trippable; pick this when piping to another tool / asserting in tests
          - text

      --kind <KIND>
          Comma-separated list of node kinds (lowercase, e.g. `function,method`) to keep on the *target* side of incoming/outgoing edges

      --file <FILE_PATH>
          Substring filter applied to the target file path of incoming/outgoing edges. Case-sensitive substring match (not glob)

      --relation_types <RELATION_TYPES>
          Comma-separated list of relation types (lowercase, e.g. `calls,imports`)

      --include_tests
          Include edges whose target lives in a test file. Defaults to false

      --graph <GRAPH>
          Path to the graph.bin file
          
          [default: .ecp/graph.bin]

  -h, --help
          Print help (see a summary with '-h')
