Locate symbols by exact name (default), substring (`--mode fuzzy`), or BM25 ranking (`--mode bm25`).

Exact / fuzzy return a single most-likely definition (or all via `--all`); bm25 returns top-K partitioned into source / tests / reference / document / config buckets and supports stdin `--batch`.

Usage: ecp find [OPTIONS] [PATTERN]

Arguments:
  [PATTERN]
          Pattern: symbol name (or name fragment in `fuzzy` / `bm25` mode). Required unless `--batch` is set (`bm25` mode only — patterns then come from stdin)

Options:
      --mode <MODE>
          Lookup mode: `exact` (default), `fuzzy`, or `bm25`

          Possible values:
          - exact: Exact-name match. Single most-likely definition by default; `--all` returns all exact matches
          - fuzzy: Substring match — same ranking + output shape as `exact`. Use when the precise name is unknown but a fragment is
          - bm25:  BM25 lexical ranking via tantivy. Bucketed top-K output
          
          [default: exact]

      --fuzzy
          Shorthand for `--mode fuzzy`. Ignored when `--mode` is supplied explicitly with a non-default value

      --all
          Return all matches instead of the single top-ranked one. Affects `exact` and `fuzzy` modes; `bm25` always returns top-K buckets

      --include-tests
          Include hits from test files in `fuzzy` mode (skipped by default). `exact` mode automatically searches tests without needing this flag. `bm25` mode bucketises into a separate `tests` array and is unaffected by this flag

      --kind <KIND>
          Filter by node kinds (csv: function,method,class,...)

      --file <FILE>
          Disambiguate when name has multiple matches: substring on file path. Same matching rule as `ecp impact --file`

      --repo <REPO>
          Repository selector (path | name | @all | csv mix). Defaults to cwd. `@<group>` is rejected at the top level — use `ecp group find` instead

      --format <FORMAT>
          Output format. Absent or `llm`: the LLM-tuned toon payload. `toon`: uncompressed toon. `json`: full-fidelity JSON. `text`: one `[kind] file:line (name)` line per match

          Possible values:
          - llm:  Default output: per-command LLM-tuned payload (token compression, rounded floats, trimmed timestamps, etc.) rendered through the toon encoder. Commands without command-specific tuning fall through to the same output as `Toon`
          - toon: Neutral toon: the json payload encoded as toon, no lossy compression. Use when you want round-trippable structure but tighter than JSON
          - json: Full-fidelity JSON. Always round-trippable; pick this when piping to another tool / asserting in tests
          - text

      --batch
          Read patterns from stdin (`bm25` mode only — one per line, lines starting with `#` or empty are skipped). Engines are loaded once outside the per-query loop so mmap setup + rkyv access are amortised across queries. Each query is emitted as a separate block prefixed by `=== pattern: <pattern> ===`

      --graph <GRAPH>
          Path to the graph.bin file
          
          [default: .ecp/graph.bin]

  -h, --help
          Print help (see a summary with '-h')
