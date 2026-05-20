List HTTP routes; with path, show handler + caller chain

Usage: ecp routes [OPTIONS] [PATH]

Arguments:
  [PATH]  If given, show handler + caller chain for this route path. If omitted, list all routes

Options:
      --method <METHOD>  Filter by HTTP method (GET / POST / PATCH / DELETE / ...)
      --repo <REPO>      Repository selector
      --depth <DEPTH>    Max depth for upstream caller traversal (only applies when <path> is set) [default: 3]
      --include-tests    Include routes declared inside test files (`tests/`, `test/`, `*_test.*`, `*.spec.*`, etc.). Default: off — most agent queries want production routes, not test fixtures. When set, the output gains a `test_results` array listing the test-only routes alongside the regular `results`. Test classification reuses `File.category = FileCategory::Test` set at index time (`ecp-analyzer/src/resolution/builder.rs:32`)
      --format <FORMAT>  Output format (toon / json / text)
      --graph <GRAPH>    Path to the graph.bin file [default: .ecp/graph.bin]
  -h, --help             Print help
