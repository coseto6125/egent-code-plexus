Build or refresh the graph (explicit / bulk)

Usage: ecp admin index [OPTIONS] --repo <REPO>

Options:
      --repo <REPO>                    
      --force                          Force-rebuild L2 at the target SHA. Drops the existing L2 dir and any orphan `.building/`, invalidates L1 sessions that have overlays for this SHA (clean sessions kept), drops the per-file `parse_cache/` so cached parser outputs from earlier binaries don't replay, then rebuilds. Without `--force`, an existing L2 is reused. Use after analyzer/grammar upgrade or to recover from L2 corruption
      --dump-resolver <DUMP_RESOLVER>  Optional path to write a JSONL dump of every resolver decision. Used by the oracle verification harness; off by default. Spec: docs/specs/2026-05-15-resolver-oracle-harness.md
      --graph <GRAPH>                  Path to the graph.bin file [default: .ecp/graph.bin]
  -h, --help                           Print help
