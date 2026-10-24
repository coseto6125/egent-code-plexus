Edge-level resolver delta — binding tier-degradation (silent break), route / contract changes.

For symbol blast-radius, use `ecp impact`.

Usage: ecp diff [OPTIONS] --section <SECTION>

Options:
      --section <SECTION>
          Comma-separated section(s) to diff: bindings, routes, contracts, symbols, or all
          
          [possible values: bindings, routes, contracts, symbols, all]

      --baseline <BASELINE>
          Git ref to compare against: branch / tag / commit SHA / HEAD~N / PR/<n>. Required unless `--baseline-graph` is supplied (A-mode snapshot diff)

      --baseline-graph <BASELINE_GRAPH>
          A-mode: path to baseline `graph.bin` (skip git checkout + re-index). When set, requires `--current-graph` and restricts sections to those that read directly from graph.bin (routes / contracts / symbols)

      --current-graph <CURRENT_GRAPH>
          A-mode: path to current `graph.bin`. Required when `--baseline-graph` is supplied

      --format <FORMAT>
          Output format. Absent or `llm`: the LLM-tuned toon payload. `toon`: uncompressed toon. `json`: full-fidelity JSON. `text`: per-section change counts and up to 10 changes per kind (all with `--verbose`)

          Possible values:
          - llm:  Default output: per-command LLM-tuned payload (token compression, rounded floats, trimmed timestamps, etc.) rendered through the toon encoder. Commands without command-specific tuning fall through to the same output as `Toon`
          - toon: Neutral toon: the json payload encoded as toon, no lossy compression. Use when you want round-trippable structure but tighter than JSON
          - json: Full-fidelity JSON. Always round-trippable; pick this when piping to another tool / asserting in tests
          - text

      --verbose
          List every change (text format only). Default truncates to top-10 per section

      --repo <REPO>
          Repository root path (defaults to current directory)

      --graph <GRAPH>
          Path to the graph.bin file
          
          [default: .ecp/graph.bin]

  -h, --help
          Print help (see a summary with '-h')
