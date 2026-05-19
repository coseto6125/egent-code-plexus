Show symbol's full context: signature, body, edges, callers, overrides, and 1-hop upstream impact

Usage: gnx inspect [OPTIONS]

Options:
      --name <NAME>
          Name of the symbol to inspect
      --repo <REPO>
          Repository path
      --format <FORMAT>
          Output format
      --kind <KIND>
          Comma-separated list of node kinds (lowercase, e.g. `function,method`) to keep on the *target* side of incoming/outgoing edges
      --file_path <FILE_PATH>
          Substring filter applied to the target file path of incoming/outgoing edges. Case-sensitive substring match (not glob)
      --relation_types <RELATION_TYPES>
          Comma-separated list of relation types (lowercase, e.g. `calls,imports`)
      --include_tests
          Include edges whose target lives in a test file. Defaults to false
      --graph <GRAPH>
          Path to the graph.bin file [default: .gnx/graph.bin]
  -h, --help
          Print help
