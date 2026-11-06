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

## 4. `status: unverified` or a warm-attach `result` line → stale warm-attach
- `l2.warm-attach` on stderr means HEAD has no published graph yet and a sibling commit's graph was attached. The overlay re-parses the files that differ from HEAD; when it covers them all, the answer is HEAD's and carries no warm-attach line.
- When it cannot cover them (too many changed files, `git diff` failed), the `result` field keeps the warm-attach line, and an exact miss reads `found: null`, `status: unverified`, with `reason` and `next`.
- **Do:** run the `next` command (`ecp admin index --force --repo .`), or rerun after the background rebuild, then re-query for a definitive answer.

## 5. `ecp impact` returned fewer callers than expected
- The caller set is a lower bound — a bare call to a name with several same-named definitions is suppressed rather than mis-attributed.
- **Do, when the result carries the ambiguity caveat:** run the command the caveat prints. It adds the `--file` or `--kind` filter a colliding name needs, plus `--ambiguous-callers`, and lists the call sites the graph could not attribute, as text-match candidates (at most 50).
- **Do, when there is no caveat, or for Ruby calls without parentheses:** `grep` the call sites before a refactor. Count same-named defs with `ecp cypher 'MATCH (n) WHERE n.name = "<name>" RETURN count(n)'`.

## 6. A symbol-type ecp doesn't capture yet
- Dead **by design**, not a freshness issue: function-body **locals** (intentionally dropped), Java `record`, PHP in-class `trait use` composition, C# `operator` / `event` / `indexer` / `destructor`.
- **Do:** confirm coverage with `ecp summary` / `ecp schema blindspots`; for these, grep is the correct fallback.

## 7. Surprising output — find the root cause before calling it a bug
- **Do:** read the actual definition, run a fresh reindex, or grep to cross-check. doc-comment inference ≠ verification; a passing `parse_file` unit test ≠ the indexing pipeline uses that path. Confirm with a fresh query against a rebuilt graph.

## 8. The hook's `ecp graph delta` looked like the whole caller set
- After a `rg` / `grep` / `egrep` / `git grep` call or a Grep tool call, the PostToolUse hook names the definitions and the direct (d=1) non-test caller files that the graph holds and the search output does not show. It prints nothing when the output shows all of them.
- **Do:** run `ecp impact --target <name> --direction upstream` before you size a refactor from the search output.
