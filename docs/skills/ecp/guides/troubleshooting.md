# Guide: Troubleshooting misses & untrustworthy results

Use this guide when `ecp` can't find a symbol you know exists, or when a
result looks wrong before you act on it. §1–3 resolve real misses; §4–8 are
the resolution steps for the five tells named in @ECP.md §"Reading output" —
the *what to do* once you've spotted one.

## 1. Check Index Freshness
- `ecp` usually auto-refreshes. If it didn't, run [`ecp admin index --repo . --force`](../_shared/refs/indexing.md). `--repo` is required — pass `.` for cwd or an absolute path.

## 2. Fuzzy Match
- Try [`ecp find <FRAGMENT> --mode fuzzy`](../_shared/cli/find.md).
- Typos or different naming conventions in different languages can cause exact-match misses.

## 3. Check Summary
- Run [`ecp summary`](../_shared/cli/summary.md).
- Look for `BlindSpots` or unparsed files. If a file is too large or uses unsupported syntax, it might be skipped.

## 4. `found:false` with a `result` field → stale warm-attach
- The tell (`result` field / `l2.warm-attach` on stderr) means HEAD has no published graph yet and a sibling commit's graph was attached; a symbol added since that sibling reads `found:false`.
- **Do:** rerun (a background rebuild may have finished), or `ecp admin index --force --repo .`, then re-query for a definitive answer.

## 5. `ecp impact` returned fewer callers than expected
- The caller set is a lower bound — a bare call to a name with several same-named definitions is suppressed rather than mis-attributed.
- **Do:** rerun with `ecp impact --target <name> --ambiguous-callers`: it lists the call sites the graph could not attribute (text-match candidates, at most 50). Ruby calls without parentheses are not matched; `grep` those. Count same-named defs with `ecp cypher 'MATCH (n) WHERE n.name = "<name>" RETURN count(n)'`.

## 6. A symbol-type ecp doesn't capture yet
- Dead **by design**, not a freshness issue: function-body **locals** (intentionally dropped), Java `record`, PHP in-class `trait use` composition, C# `operator` / `event` / `indexer` / `destructor`.
- **Do:** confirm coverage with `ecp summary` / `ecp schema blindspots`; for these, grep is the correct fallback.

## 7. Surprising output — find the root cause before calling it a bug
- **Do:** read the actual definition, run a fresh reindex, or grep to cross-check. doc-comment inference ≠ verification; a passing `parse_file` unit test ≠ the indexing pipeline uses that path. Confirm with a fresh query against a rebuilt graph.

## 8. The hook's `ecp graph hits` looked like the whole caller set
- The PreToolUse hook prints direct (d=1) callers only, to stay fast on every tool call.
- **Do:** run the `ecp impact --target <name> --direction upstream` line the hook prints before you size a refactor from it.
