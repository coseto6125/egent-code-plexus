# cgn Indexing

Indexing in Code Graph Nexus is designed to be low-friction and mostly automatic.

## Automatic Refreshes
Agent commands (like `inspect`, `find`, `impact`) auto-detect stale or missing graphs and rebuild on demand. You will see a stderr line:
`✓ Index refreshed (... in Xs)`

## Manual Indexing
If you need to force a full re-index or are setting up a repo for the first time:
```bash
cgn admin index --repo <PATH> [--force]
```

## Performance
- **Sub-second** incremental rebuilds for small changes.
- **30s – 2min** for full initial index of typical repositories.
- Uses Rust's Rayon for parallel parsing.
