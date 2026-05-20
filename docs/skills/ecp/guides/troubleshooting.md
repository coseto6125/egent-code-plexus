# Guide: Troubleshooting "Not Found"

Use this guide if `ecp` cannot find a symbol that you know exists in the source.

## 1. Check Index Freshness
- `ecp` usually auto-refreshes. If it didn't, run [`ecp admin index --repo . --force`](../_shared/refs/indexing.md).

## 2. Fuzzy Match
- Try [`ecp find <FRAGMENT> --mode fuzzy`](../_shared/cli/find.md).
- Typos or different naming conventions in different languages can cause exact-match misses.

## 3. Check Coverage
- Run [`ecp coverage`](../_shared/cli/coverage.md).
- Look for `BlindSpots` or unparsed files. If a file is too large or uses unsupported syntax, it might be skipped.
