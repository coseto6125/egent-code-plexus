BM25 symbol lookup across all group members. `--merge none` (default) emits per-repo bucketed concat; `--merge rrf --limit N` returns a unified top-K via Reciprocal Rank Fusion. `--batch` reads patterns from stdin and re-applies the merge mode per pattern

Usage: ecp group find [OPTIONS] <NAME> [PATTERN]

Arguments:
  <NAME>
          Group name

  [PATTERN]
          BM25 pattern (symbol name or fragment). Required unless `--batch`

Options:
      --merge <MERGE>
          Result assembly mode

          Possible values:
          - none: Per-repo bucketed concat — every member's hits emitted under its own header. Matches the single-repo `ecp find --mode bm25` shape
          - rrf:  Reciprocal Rank Fusion across repos → unified top-K. Ranking is by `Σ_repo 1 / (RRF_K + rank + 1)` over `Hit.signature` as the dedupe key
          
          [default: none]

      --limit <LIMIT>
          Top-K results (only meaningful with `--merge rrf`; rejected otherwise)

      --batch
          Read patterns from stdin, one per line. Lines starting with `#` or empty after trim are skipped. Each pattern emits a `=== pattern: <p> ===` divider so downstream scripts can split per-query

      --json
          JSON output

      --graph <GRAPH>
          Path to the graph.bin file
          
          [default: .ecp/graph.bin]

  -h, --help
          Print help (see a summary with '-h')
