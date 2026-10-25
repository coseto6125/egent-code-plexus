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
          The kind of the declaration to inspect, one kind only (for example `function`, `method`, `impl`). The command selects only the declarations of this kind. A comma-separated list is an error. To keep only neighbours of some kinds, use `--neighbor-kind`

      --file <FILE_PATH>
          The file of the declaration to inspect: a path or a part of a path. The command selects the declarations in the best match: the equal path, then a path that ends with `/<FILE>`, then a path that holds `<FILE>`. To keep only neighbours in some files, use `--neighbor-file`

      --neighbor-kind <NEIGHBOR_KIND>
          Comma-separated node kinds (lowercase, for example `function,method`). The output keeps only the edges whose other end has one of these kinds. This flag does not change which declaration the command selects

      --neighbor-file <NEIGHBOR_FILE>
          A part of a path (case-sensitive, not a glob). The output keeps only the edges whose other end is in a file whose path holds this text. This flag does not change which declaration the command selects

      --relation_types <RELATION_TYPES>
          Comma-separated list of relation types (lowercase, e.g. `calls,imports`)

      --include_tests
          Include edges whose target lives in a test file. Defaults to false

      --graph <GRAPH>
          Path to the graph.bin file
          
          [default: .ecp/graph.bin]

  -h, --help
          Print help (see a summary with '-h')
