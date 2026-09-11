# ecp review

LLM-workflow audit aggregator. Runs multiple checks in one shot.

## Usage
```bash
ecp review --baseline origin/main
ecp review --baseline origin/main --include flow
```

## Checks Performed
- `impact`: Blast radius of changed symbols.
- `summary`: Check for blind spots in changed files.
- `tool-map`: Egress changes (new external calls).
- `shape-check`: Route response drift.
- `diff`: Resolver binding tier degradation.
- `literal-coherence`: Path-literal split-brain — a writer emitting one filename
  while a reader opens a near-identical name (e.g. `session_meta.json` vs
  `meta.json`). Graph-wide scan, so it fires even when only one side changed.

## Best For
- PR pre-check.
- Getting a high-signal summary of structural changes.

## Value changes

`--include flow` adds source-based before/current consumer analysis to the existing findings.
The baseline defaults to `HEAD`. Working-tree edits participate in the current flow snapshot.
Use `--files` to select changed files while retaining dependency sources for analysis.
When flow evidence needs compatibility review, `status` becomes `review_required` and `legacy_status` retains the graph result.
This status is not a bug verdict. Inspect consumers, unresolved boundaries, and truncation before drawing conclusions.
`--include flow` cannot be combined with `--verdicts`.
See [flow](flow.md) for supported sources, evidence, and limits.
