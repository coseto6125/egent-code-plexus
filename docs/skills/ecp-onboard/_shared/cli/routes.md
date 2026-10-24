List HTTP routes; with path, show handler + caller chain

Usage: ecp routes [OPTIONS] [PATH]

Arguments:
  [PATH]
          If given, show handler + caller chain for this route path. If omitted, list all routes

Options:
      --method <METHOD>
          Filter by HTTP method (GET / POST / PATCH / DELETE / ...)

      --repo <REPO>
          Repository selector

      --depth <DEPTH>
          Max depth for upstream caller traversal (only applies when <path> is set)
          
          [default: 3]

      --include-tests
          Include routes declared inside test files (`tests/`, `test/`, `*_test.*`, `*.spec.*`, etc.). Default: off — most agent queries want production routes, not test fixtures. When set, the output gains a `test_results` array listing the test-only routes alongside the regular `results`. Test classification reuses `File.category = FileCategory::Test` set at index time (`ecp_core::file_category::determine_category`)

      --format <FORMAT>
          Output format. Absent or `llm`: the LLM-tuned toon payload. `toon`: uncompressed toon. `json`: full-fidelity JSON. `text`: one compact JSON line per route. Absent, `llm` and `toon` join the middleware names of a route into one string

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
