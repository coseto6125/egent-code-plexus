# ecp flow

Trace possible value dependencies from a source position. The query reads current sources without loading or changing `graph.bin`.

## Usage

```bash
ecp flow --file blocks.js --line 87 --column 12 --subject return --format json
ecp flow --file src/view.ts --line 20 --column 7 --subject binding --direction forward
ecp flow --file src/view.ts --line 31 --column 14 --direction backward
ecp flow --file src/view.ts --line 31 --column 14 --overlay /tmp/sources.json
ecp review --baseline origin/main --include flow
```

Paths are relative to `--repo`, which defaults to the current directory.
Lines and UTF-8 byte columns start at one. Select the exact expression or binding.
The overlay is a JSON object mapping repository-relative paths to complete unsaved source strings.
It changes the analysis snapshot, not files on disk.

## Select the question

| Subject | Meaning |
| --- | --- |
| `value` | The selected expression's result |
| `binding` | The selected variable binding |
| `return` | The enclosing function's return values |

Forward tracing finds consumers. Backward tracing finds origins.
Calling a function, passing the function itself, and using its return value are separate operations.
A call that discards its return value does not create a return-value dependency in its caller.

## Evidence and limits

The engine analyzes JS/TS, Python, and PHP sources within the supplied directory.
Ignore rules filter untracked files. Tracked sources remain visible in both snapshots, subject to dependency/cache directory exclusions.
Missing external code remains an analysis boundary.
The loader rejects scopes larger than 2,048 files or 32 MiB instead of silently analyzing a partial snapshot.
Overlay JSON input also has a 32 MiB limit before parsing.

The report contains source hashes, selected anchors, dependency nodes and edges, consumers, boundaries, and truncation status.
Node locations identify the evidence. Value and control dependencies use different edge kinds.
Boundaries explain unresolved behavior. They are not proof of absence.
The result identifies possible dependencies, not guaranteed runtime changes or a compatibility verdict.

The engine distinguishes lexical bindings and assignment versions, merges branch states, and expands calls by call site.
Loops use bounded fixed-point iteration. Recursive value dependencies use cyclic summaries.
Named JS/TS imports and reexports, plus Python from-imports, connect supported values across files.
Closure captures and tracked object aliases connect values across functions.
PHP includes and namespace imports remain explicit boundaries.
Selected numeric builtins have explicit models. Their assumptions appear as `builtin_model` boundaries.
Destructuring, asynchronous scheduling, exception paths, iterator protocols, and unresolved external effects retain explicit boundaries.
Recursive heap and callable identities remain approximate. These limits apply even when `truncated` is false.

Use `--max-nodes`, `--max-call-depth`, and `--max-steps` to bound analysis.
When a budget stops analysis, inspect the boundary and truncation fields before making a claim.
An empty consumer list establishes no broader conclusion than the reported coverage permits.

The CLI caches the latest result per repository under the ecp home directory.
All source snapshots, overlay mode, analyzer fingerprint, dependency lockfile, and query options participate in cache invalidation.
Use `--no-cache` to bypass it. A cache write failure does not prevent analysis.

## Review and agent integration

`ecp review --baseline <ref> --include flow` adds changed-value evidence to the existing graph review.
The aggregate review retains its existing index requirements. Standalone `flow` needs no index.
Before and current snapshots remain separate, including dependencies in other files.
A changed operator can require consumer review even when dependency edges stay the same.

MCP exposes `ecp_flow` from the same CLI schema and executes the same command.
Use the normal ecp skill for routing. Check each consumer and boundary before declaring compatibility.
Refresh analysis after source changes rather than carrying forward old conclusions.

Claude Code Edit/Write hooks provide bounded before/after evidence for the edited file only, when installed. Consumers in other files are not in hook evidence; run `ecp review --include flow --baseline <ref>` for them.
Other hosts must use the explicit flow/review commands unless their adapter supports the actual edit event.
Hook evidence is a summary. Read the complete result when it is truncated or unresolved.
