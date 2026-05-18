Registry + repo health (indexed repos, freshness, frameworks, blind spots). External-client (HTTP/DB/Redis/queue) usage detail: see `gnx tool-map`

Usage: gnx coverage [OPTIONS]

Options:
      --repo <REPO>      Repository selector (path | name | @group | @all | csv mix). If omitted: registry-level overview only
      --detailed         Verbose per-section breakdown (include branch rows, etc.)
      --format <FORMAT>  Output format. Omit for the LLM-tuned default (toon-encoded, lossy confidence rounding + compact timestamps). `--format toon` is the neutral toon encoding of the full payload; `--format json` is the full-fidelity JSON; `--format text` is the human-friendly fallback
      --graph <GRAPH>    Path to the graph.bin file [default: .gnx/graph.bin]
  -h, --help             Print help
