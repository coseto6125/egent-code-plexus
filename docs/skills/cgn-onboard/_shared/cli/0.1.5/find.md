Locate symbols by exact name (default), substring (`--mode fuzzy`), or BM25 lexical ranking (`--mode bm25`). Exact / fuzzy return a single most-likely definition (or all via `--all`); bm25 returns top-K partitioned into source / tests / reference / document / config buckets and supports stdin `--batch`

Usage: cgn find [OPTIONS] [PATTERN]

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
          Include hits from test files in `exact` / `fuzzy` modes (skipped by default). `bm25` mode bucketises into a separate `tests` array and is unaffected by this flag

      --kind <KIND>
          Filter by node kinds (csv: function,method,class,...)

      --repo <REPO>
          Repository selector (path | name | @all | csv mix). Defaults to cwd. `@<group>` is rejected at the top level — use `cgn group find` instead

      --format <FORMAT>
          Output format: text (default) | json | toon

      --batch
          Read patterns from stdin (`bm25` mode only — one per line, lines starting with `#` or empty are skipped). Engines are loaded once outside the per-query loop so mmap setup + rkyv access are amortised across queries. Each query is emitted as a separate block prefixed by `=== pattern: <pattern> ===`

      --graph <GRAPH>
          Path to the graph.bin file
          
          [default: .cgn/graph.bin]

  -h, --help
          Print help (see a summary with '-h')
