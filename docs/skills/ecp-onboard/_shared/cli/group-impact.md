Local blast-radius for one member, fanned out via cross-repo links

Usage: ecp group impact [OPTIONS] --target <TARGET> --repo <REPO> <NAME>

Arguments:
  <NAME>  Group name

Options:
      --target <TARGET>
          Symbol name (function/method/file) to analyse
      --repo <REPO>
          Member name within the group (dir_name or alias)
      --direction <DIRECTION>
          Upstream (callers) or downstream (callees) [default: upstream]
      --max-depth <MAX_DEPTH>
          Local-impact max graph traversal depth
      --cross-depth <CROSS_DEPTH>
          Cross-repo hop depth (clamped to 1 in first wave)
      --min-confidence <MIN_CONFIDENCE>
          Minimum cross-link confidence to surface
      --timeout-ms <TIMEOUT_MS>
          Local-impact wall-clock budget in ms
      --include-tests
          Include test files in local traversal
      --json
          JSON output instead of TOON
      --graph <GRAPH>
          Path to the graph.bin file [default: .ecp/graph.bin]
  -h, --help
          Print help
