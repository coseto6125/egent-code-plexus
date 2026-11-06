Symbol blast radius — affected callers (counts + call sites).

For binding tier-degradation or resolver delta, use `ecp diff`.

Usage: ecp impact [OPTIONS] [NAME]

Arguments:
  [NAME]
          Target symbol name (mutually exclusive with --baseline). Equivalent to the `--target` named form below. Optional when `--batch` is set or when a non-positional mode flag (`--target`, `--baseline`, `--literal`, `--literal-coherence`) supplies the query

Options:
      --target <TARGET>
          Named alias for the positional NAME argument — kept for parity with old MCP / wrapper habits

      --baseline <BASELINE>
          Git ref — compute blast radius across all symbols changed between this baseline and HEAD. Mutually exclusive with positional <name>

      --file <FILE>
          Select the declaration in this file. A file path that equals the value ranks first, then one that ends with it, then one that contains it. An ambiguous name returns candidates that you can re-run. `--file_path` / `--file-path` stay as aliases for back-compat

      --kind <KIND>
          Select the declaration of one node kind (function | method | class | ...). The kind filter applies before `--file` ranks the declarations

      --direction <DIRECTION>
          Direction of traversal
          
          [default: up]
          [possible values: up, down, both]

      --depth <DEPTH>
          Maximum BFS depth
          
          [default: 5]

      --high-trust-only <HIGH_TRUST_ONLY>
          Default OFF — recall-first: traverse every edge regardless of confidence (cross-crate refs at 0.7 are still real callers, just less certain). Pass `--high-trust-only=true` to restrict to confidence ≥ 0.8 edges for a noise-light view; when filtering kicks in, the output reports `hidden_edges` so missed coverage stays visible
          
          [default: false]
          [possible values: true, false]

      --min-confidence <MIN_CONFIDENCE>
          Override the high-trust threshold with a custom value (0.0–1.0). If set, takes precedence over --high-trust-only

      --include-tests
          Test callers are listed by default, tagged `test: true`, since a rename breaks them too. A test caller sits in a test file or carries a test attribute (`#[test]`, `@Test`, `[Fact]`); a test-like name alone is none. This flag is kept for old callers; on its own it changes one thing: `--baseline` also treats changed test files as changed symbols

      --exclude-tests
          Drop test callers; the payload reports `hidden_test_callers: N`

      --relation_types <RELATION_TYPES>
          Comma-separated relation types to follow (calls, extends, ...)

      --repo <REPO>
          Repository selector

      --test-coverage
          Coverage gap analysis: for each touched symbol, classify by verified (untagged) test callers (uncovered / partial / covered / orphan). A test caller tagged `requires_verification` counts under `unverified_test_callers`, never as coverage. Orphan means no caller at all. Uses FunctionMeta.is_test flag from per-language extraction. Outputs uncovered symbols first to support LLM PR review ("X 改了沒測試"). Needs test callers in the walk, so it conflicts with --exclude-tests

      --no-heuristic
          Suppress heuristic callers (MirrorsField, EventTopicMirror, CompensatedBy) from the blast radius. Default: heuristic callers ARE shown, in a separate `heuristic_callers` bucket tagged `requires_verification`. With this flag, `Calls` callers below 0.7 confidence still appear, tagged `requires_verification`

      --confidence-threshold <CONFIDENCE_THRESHOLD>
          Informational confidence gate — promotes heuristic edges when T4-7/T5-33 emit per-edge tiers. Currently controls the --explain-confidence report
          
          [default: 0.85]

      --explain-confidence
          Emit explain_confidence block with threshold + per-tier filtered counts

      --format <FORMAT>
          Output format. Absent or `llm`: the LLM-tuned toon payload. `toon`: uncompressed toon. `json`: full-fidelity JSON. `text`: pretty-printed JSON. Every impact row carries `requires_verification`: true when an edge on its path is heuristic or below 0.7 confidence (the row keeps its place). `ownerClass` is the stored owner, null for a nested fn or closure. `lexicalParent` is the enclosing chain (`K::m`), else the stored owner. `unexplained_refs` is present when the text scan ran (see --ambiguous-callers). `blind_spot_warning.files` is an array: upstream, the files of the `unexplained_refs` sites; downstream, the target's own file. An ambiguous target exits non-zero and prints `status: "ambiguous"` with at most 5 `candidates`, most `callers` first, each with `target`, `file`, `line`, `kind`, `callers` and `top_callers`; `more` and `more_command` name the candidates not shown

          Possible values:
          - llm:  Default output: per-command LLM-tuned payload (token compression, rounded floats, trimmed timestamps, etc.) rendered through the toon encoder. Commands without command-specific tuning fall through to the same output as `Toon`
          - toon: Neutral toon: the json payload encoded as toon, no lossy compression. Use when you want round-trippable structure but tighter than JSON
          - json: Full-fidelity JSON. Always round-trippable; pick this when piping to another tool / asserting in tests
          - text

      --literal <VALUE>
          List sites of a path-shaped string literal by exact value. Mutually exclusive with --target/--baseline/<name>. Returns JSON with each site's file, line, enclosing fn, and sink classification (`sink:read` / `sink:write` / `sink:open-read` / `sink:join` / etc). Designed for LLM split-brain queries: `ecp impact --literal session_meta.json` answers "where is this file read or written?" without writing cypher

      --literal-coherence
          Auto-detect likely path-literal split-brain pairs across all PathLiteral nodes. Conservative: same extension, similar basename, nearby directories, and read-only vs write-only sink separation

      --batch
          Read target symbol names from stdin (one per line; `#` and blank lines skipped). The graph is loaded once and N symbols are resolved sequentially — amortises mmap + process spawn across queries. Each result is prefixed by `=== target: <name> ===` so callers can split the stream unambiguously. Flags like --direction / --depth / --exclude-tests apply uniformly to all targets.
          
          Symbol-mode only: `--batch` combined with `--baseline` or `--literal` is rejected as an invalid argument. A positional name is also rejected — stdin is the single source of targets, so a positional would be silently ignored otherwise.

      --ambiguous-callers
          Force the `unexplained_refs` text scan. Without this flag the scan runs on an upstream or both walk when the target has at most 2 non-test direct callers, or shares its name with another definition of a competing kind. The scan runs one `git grep` for whole-word occurrences of the bare name in every tracked file except vendored code and build output (`node_modules/`, `vendor/`, `.venv/`, `site-packages/`, `dist/`, `build/`, `*.min.js`, lockfiles) and documents (`.md`, `.rst`, `.txt`), in every language. It lists each occurrence that no edge into a same-named definition explains, as `{path, line, form, enclosing}`: `form` is `bare` or `member` for a call, `reference` for any other use. At most 20 sites; `total` counts every occurrence and `files` the files that hold one. A site whose line holds several occurrences carries `count`. `total: 0` means the scan ran and found none; `error` says why the scan failed. Sites are text matches, never merged into the caller list or counts. With `--batch` the scan keeps its old rule: it runs only with this flag and only for a name with two or more same-named definitions. Ignored with `--direction down`

      --graph <GRAPH>
          Path to the graph.bin file
          
          [default: .ecp/graph.bin]

  -h, --help
          Print help (see a summary with '-h')
