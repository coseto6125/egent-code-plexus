Registry + repo health (indexed repos, freshness, frameworks, blind spots). `blind_spots` lists only LLM-actionable opacity (dynamic-import / reflection / eval); parser-metric buckets (uid-collision / overload / ifdef-redef) live under `ecp dev uid-audit`. External-client (HTTP/DB/Redis/queue) usage detail: see `ecp tool-map`.

Usage: ecp summary [OPTIONS]

Note: `ecp coverage` is kept as an alias for one release; new code should use `ecp summary`.

Options:
      --repo <REPO>      Repository selector (path | name | @group | @all | csv mix). If omitted: registry-level overview only
      --detailed         Verbose per-section breakdown (include branch rows, etc.)
      --format <FORMAT>  Output format. Omit for the LLM-tuned default (toon-encoded, lossy confidence rounding + compact timestamps). `--format toon` is the neutral toon encoding of the full payload; `--format json` is the full-fidelity JSON; `--format text` is the human-friendly fallback
      --graph <GRAPH>    Path to the graph.bin file [default: .ecp/graph.bin]
  -h, --help             Print help
