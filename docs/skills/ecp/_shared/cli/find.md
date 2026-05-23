# ecp find

Locate symbols by name using various matching modes.

## Usage
```bash
ecp find "pattern" [--mode <MODE>] [--repo <PATH>]
```

## Modes
- **exact** (default): Find exact name matches. Returns single top-ranked definition unless `--all` is passed.
- **fuzzy**: Substring match for partial / mistyped names. Returns single top-ranked hit (or all via `--all`) — same output shape as `exact`.
- **bm25**: Ranked lexical search via Tantivy. Returns top-K partitioned into buckets (multi-result, multi-bucket — different output shape from `exact` / `fuzzy`):
  - `source`: Production code hits.
  - `tests`: Test code.
  - `reference`: References/usages.
  - `document`: Documentation hits.
  - `config`: Configuration files.

## Options
- `--all`: Return all exact matches instead of just the top-ranked one.
- `--include-tests`: Include test files in search.
