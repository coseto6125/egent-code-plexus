Edge-level resolver delta — binding tier-degradation (silent break), route / contract changes. For symbol blast-radius, use `gnx impact`

Usage: gnx diff [OPTIONS] --section <SECTION> --baseline <BASELINE>

Options:
      --section <SECTION>    Comma-separated section(s) to diff: bindings, routes, contracts, or all [possible values: bindings, routes, contracts, all]
      --baseline <BASELINE>  Git ref to compare against: branch / tag / commit SHA / HEAD~N / PR/<n>. No default
      --format <FORMAT>      Output format. Omit for the LLM-tuned default; pass `--format text|json|toon` for the alternative renderings
      --verbose              List every change (text format only). Default truncates to top-10 per section
      --repo <REPO>          Repository root path (defaults to current directory)
      --graph <GRAPH>        Path to the graph.bin file [default: .gnx/graph.bin]
  -h, --help                 Print help
