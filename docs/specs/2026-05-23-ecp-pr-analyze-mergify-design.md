# `ecp dev pr-analyze` + Mergify integration — design

## Context

This repo wants a merge queue to handle three pains, ranked:

1. **Throughput** — multi-session Claude workflow produces 2-5 PRs simultaneously ready; FIFO `gh pr merge --squash` serialises them all behind full CI.
2. **Safety** — racing manual merges can leave `main` broken: PR A merges, PR B's CI passed before A merged, B's code uses something A removed.
3. **Convenience** — auto-merge once CI is green, no babysitting.

### Options evaluated, ranked

| Option | Verdict |
|---|---|
| GitHub native merge queue | ❌ Requires GitHub Team+ org plan; this repo is on a personal account. Verified by `createRepositoryRuleset` with `MERGE_QUEUE` rule type returning `UNPROCESSABLE: Invalid rules`. |
| Pure self-built orchestrator (Rust + GH Actions, ~400 LoC) | ❌ Overengineered for solo-dev volume; battle-tested edge cases (race/partial-fail/retry) take years of real-traffic polish. |
| Pure Mergify (`.mergify.yml` only) | ⚠️ Covers ~80% of pains in 20 lines. Loses ecp's structural advantage: semantic (call-graph) conflict detection. |
| **Mergify + `ecp dev pr-analyze` signals** ← chosen | ✅ Best of both: Mergify owns orchestration / race / retry; ecp feeds graph-aware labels + commit statuses. ~320 LoC, no orchestration to maintain. |
| Other vendors (Aviator, Trunk, Kodiak, Bulldozer) | ❌ Integration surface (labels + status checks) is identical to Mergify across all commercial competitors. Kodiak (the only OSS forkable candidate) is unmaintained as of 2026. |

## Goals

- **G1**: Mergify's batching effectively becomes graph-aware via labels — disjoint-area PRs land in parallel queues; same-area PRs serialise.
- **G2**: Cross-PR **semantic** conflict detection that pure file-path tools cannot do — block a PR if its `changed_symbols` intersect another queued PR's `impact set`.
- **G3**: Risk-based priority — low-blast-radius PRs jump ahead of high-blast-radius ones inside the same queue.
- **G4**: Dogfood ecp — the CI gate uses the same `ecp impact` query an LLM agent would run, validating that path.

## Non-goals

- Replace Mergify orchestration, retry, conflict-resolution, queue dashboard.
- Selective test runner (orthogonal; can be a separate Action later).
- Multi-repo coordination.
- Custom queue UI.
- Bisect / per-PR retry budget (Mergify handles via speculative checks).
- Auto-rebase on base drift (Mergify handles).
- Slack / email notifications (Mergify handles).

## Architecture

```
PR opened or synchronize
        │
        ▼
.github/workflows/ecp-pr-analyze.yml          (~80 LoC YAML)
        │
        ▼
cargo run --release -p egent-code-plexus --
    dev pr-analyze
        --baseline origin/main
        --pr-head HEAD
        --pr-number ${{ github.event.pull_request.number }}
        --format json
        │
        ▼
Workflow parses JSON →
    gh pr edit --add-label "ecp:area-X" "ecp:risk-Y"
    gh api ... commit-status ecp/cross-pr-conflict=success|pending
        │
        ▼
Mergify GitHub App reads labels + statuses →
    routes PR to matching queue (area-based)
    applies priority (risk-based)
    runs speculative batch trials
    merges via gh pr merge --squash --delete-branch
        │
        ▼
Mergify's own post-merge handling (next tick, conflict resolution, etc.)
```

**State**: derived entirely from PR labels + commit statuses. Both ecp and Mergify are stateless w.r.t. each other. A runner cancellation, GH outage, or workflow re-run produces the same decisions on next tick.

## Components

### 1. `crates/ecp-cli/src/commands/dev/pr_analyze.rs` (~120 LoC)

Pure subcommand under the existing `ecp dev` namespace.

**CLI surface:**

```
ecp dev pr-analyze
    --baseline <git-ref>      # required; base branch ref (typically origin/main)
    --pr-head <git-ref>       # required; PR HEAD ref
    --pr-number <n>           # required; for cross-PR conflict lookup
    --format <json|text>      # default: json
    --queue-label <label>     # default: merge-queue; used to scope cross-PR conflict scan
    [--dry-run]               # don't side-effect anything; just print would-be JSON
```

**Output JSON schema:**

```json
{
  "pr_number": 373,
  "head_sha": "0092df3c...",
  "baseline_sha": "dbb71278...",
  "area": "cli",
  "risk": "low",
  "impact_size": 7,
  "changed_symbols": ["FnA", "FnB", "StructC"],
  "cross_pr_conflicts": [
    { "pr": 374, "overlap_symbols": ["FnA"] }
  ],
  "suggested_labels": ["ecp:area-cli", "ecp:risk-low"],
  "suggested_status": {
    "context": "ecp/cross-pr-conflict",
    "state": "pending",
    "description": "Conflicts with PR #374 on FnA"
  }
}
```

**Internal flow:**

1. Invoke existing `ecp impact --baseline <baseline> --format json` (no new graph code needed).
2. Parse `changed_symbols` and full `impact_set` from impact output.
3. Classify `area` from the set of changed file paths (see §Classification).
4. Classify `risk` from `impact_size` (see §Classification).
5. For cross-PR conflict: query `gh pr list --label <queue-label> --state open --json number,headRefOid` for sibling PRs. For each, fetch its previously analyzed impact set from its `ecp/cross-pr-conflict` status description (cached payload). Compute `self.changed_symbols ∩ other.impact_set`. Report any overlap.
6. Build `suggested_labels` + `suggested_status` payload.
7. Emit JSON to stdout.

**Why "suggested" not "applied"** — the subcommand is a pure analyzer; the workflow does the GitHub mutations. This keeps `pr-analyze` testable without GH credentials and re-usable from other contexts (e.g., a local pre-push hook).

### 2. `.github/workflows/ecp-pr-analyze.yml` (~80 LoC)

**Triggers:**
- `pull_request: { types: [opened, synchronize, reopened] }`
- `pull_request_target` for fork PRs (read-only access; relevant if repo accepts external contribs in future)

**Steps:**

1. Checkout PR head with full history (`fetch-depth: 0` so `git diff origin/main..HEAD` works).
2. Cache `~/.ecp/` between runs so the graph index is incremental.
3. `cargo build --release --bin ecp` (cached by `actions/cache`).
4. Run `ecp dev pr-analyze --baseline origin/main --pr-head HEAD --pr-number $PR_NUM --format json > /tmp/analysis.json`.
5. Parse JSON via `jq`:
   - For each label in `.suggested_labels[]`: `gh pr edit $PR_NUM --add-label "$label"`.
   - Remove stale `ecp:*` labels not in suggested set (so a re-sync after a code change updates classification).
   - Push commit status via `gh api repos/$REPO/statuses/$HEAD_SHA -f context=... -f state=... -f description=...`.
6. Set `continue-on-error: true` on the analysis step — if ecp panics or graph isn't built, the workflow logs the failure but doesn't block the PR. Mergify then falls back to the `default` queue (no `ecp:*` labels = matches `default` rules).

**Permissions** required: `contents: read`, `pull-requests: write`, `statuses: write`.

### 3. `.mergify.yml` (~40 LoC)

```yaml
queue_rules:
  - name: test-only
    conditions:
      - check-success=ecp/cross-pr-conflict
      - label=ecp:area-test
    batch_size: 10
    batch_max_wait_time: 30s

  - name: parser-changes
    conditions:
      - check-success=ecp/cross-pr-conflict
      - label=ecp:area-parser
    batch_size: 1

  - name: cli-changes
    conditions:
      - check-success=ecp/cross-pr-conflict
      - label=ecp:area-cli
    batch_size: 3
    batch_max_wait_time: 2m

  - name: docs-only
    conditions:
      - check-success=ecp/cross-pr-conflict
      - label=ecp:area-docs
    batch_size: 20
    batch_max_wait_time: 10s

  - name: default
    conditions:
      - check-success=ecp/cross-pr-conflict
    batch_size: 2

pull_request_rules:
  - name: queue if labeled merge-queue
    conditions:
      - label=merge-queue
      - check-success=Test (all platforms)
      - check-success=Code Quality (Linting & Formatting)
      # (other required contexts from current branch protection)
    actions:
      queue:
        name: default

  - name: high priority for low-risk
    conditions:
      - label=ecp:risk-low
    actions:
      queue:
        priority: high

  - name: low priority for high-risk
    conditions:
      - label=ecp:risk-high
    actions:
      queue:
        priority: low
```

## Data flow (single PR lifecycle)

```
1. Author opens PR #X off feat/topic
        ▼
2. push event → CI matrix runs (existing branch protection contexts)
        ▼
3. push event → ecp-pr-analyze.yml runs
        - ecp dev pr-analyze emits JSON
        - workflow applies ecp:area-cli + ecp:risk-low labels
        - workflow posts commit status ecp/cross-pr-conflict=success
        ▼
4. Author (or another session) adds label `merge-queue`
        ▼
5. Mergify sees: merge-queue + all required checks success + ecp/cross-pr-conflict=success
        → enqueues into "cli-changes" queue with priority=high
        ▼
6. Mergify speculatively merges with other batch-mates (up to 3 in cli queue)
        ▼
7. Speculative CI passes → Mergify merges all batch via `gh pr merge --squash --delete-branch`
        ▼
8. Push to main triggers ecp-pr-analyze.yml on all OTHER open PRs
        → if their impact intersects merged content, their ecp/cross-pr-conflict flips to pending
        → those PRs leave the queue automatically until next analysis clears them
```

## Classification rules

### Area classification (path-based, deterministic)

| All changed paths match | → Label |
|---|---|
| `crates/ecp-analyzer/src/<lang>/**` | `ecp:area-parser` |
| `crates/ecp-cli/src/commands/**` | `ecp:area-cli` |
| `crates/ecp-cli/tests/**` OR `crates/ecp-analyzer/tests/**` OR `**/examples/**` | `ecp:area-test` |
| `docs/**` OR `*.md` OR `README*` | `ecp:area-docs` (highest priority, smallest batch trial cost) |
| Anything mixed across categories | _no area label_ → falls to `default` queue |

Rationale: only assign an area label when ALL paths agree. Mixed PRs go to `default` (most conservative batching) because the area-specific queues' batch_size assumptions don't hold for them.

### Risk classification (impact-set-based, graph-driven)

- `ecp:risk-low`: `impact_size ≤ 5` callers
- `ecp:risk-medium`: `5 < impact_size ≤ 30`
- `ecp:risk-high`: `impact_size > 30`

Thresholds are dev-machine measurements on this repo's current codebase; revisit after one month of usage data. Tune via a config file later (out of scope now).

`impact_size` is the total node count returned by `ecp impact --baseline origin/main --direction up --depth 5` (upstream callers, default settings).

### Cross-PR conflict detection (semantic, graph-driven — the key ecp differentiator)

**Algorithm:**

```
self.changed_symbols = symbols modified in this PR's diff
self.impact_set = closure(self.changed_symbols) via ecp impact

for other_pr in gh pr list --label merge-queue --state open:
    if other_pr.head_sha == self.head_sha:
        continue

    other.impact_set = fetch_cached_impact_comment(other_pr)  # parses <!-- ecp-impact-cache:V1 --> marker comment
    if other.impact_set is None:
        # other PR not yet analyzed; defer judgement, treat as conflict
        report_conflict(other_pr, "pending analysis")
        continue

    overlap = self.changed_symbols ∩ other.impact_set
    if overlap is non-empty:
        report_conflict(other_pr, overlap)
```

**Caching mechanism**: each PR carries a single hidden PR comment with marker `<!-- ecp-impact-cache:V1 -->` followed by JSON-encoded impact set (symbol names, deduplicated). Subsequent analyses on sibling PRs fetch and parse this comment. No external store needed.

```
# Read sibling PR's cached impact
gh api repos/$REPO/issues/$PR_NUM/comments --jq \
  '.[] | select(.body | startswith("<!-- ecp-impact-cache:V1 -->")) | .body'

# Write own cache (update existing or create new)
existing_id=$(gh api ... | jq -r '.id')
if [ -n "$existing_id" ]; then
    gh api -X PATCH repos/$REPO/issues/comments/$existing_id -f body="..."
else
    gh pr comment $PR_NUM --body "..."
fi
```

Why PR comments not commit status description: GitHub commit status `description` is capped at 140 characters — insufficient for impact JSON. PR comments have no practical size limit and are queryable by REST API.

**Race condition**: if two PRs are pushed within the same second, both analyses run in parallel and neither sees the other's cached impact yet. Worst case: both pass `cross-pr-conflict=success`; Mergify queues both; speculative test catches actual conflict. Same protection as Mergify-alone, no regression.

## Error handling

| Scenario | Behavior |
|---|---|
| ecp graph not yet built / impact subcommand fails | Workflow logs warning, applies no `ecp:*` labels, no commit status. PR falls to `default` queue. Conservative degradation. |
| `pr-analyze` panics | `continue-on-error: true` on analysis step; CI matrix continues to gate via existing branch protection. Human can still manual-merge. |
| Two `ecp-pr-analyze` workflows race | Each independently writes labels / status to its own PR. No shared state. Worst case: both miss each other's just-cached impact (see above). |
| Mergify not installed / `.mergify.yml` syntax error | Labels and statuses are still set but unused. Existing manual `gh pr merge` workflow unaffected. |
| PR is a docs-only fork PR with no permission to set labels | `gh pr edit` fails; workflow continues. PR reviewed manually. |
| Cached impact comment exceeds 65535-char GH comment limit | Truncate symbol list to top-N by reverse-frequency (most-referenced first); mark cache marker as `<!-- ecp-impact-cache:V1:truncated -->` so consumers know precision is partial. |

## Testing

### Unit tests (`crates/ecp-cli/src/commands/dev/pr_analyze/tests.rs`)

- `classify_area_pure_parser` — all paths under `crates/ecp-analyzer/src/python/` → `Some("parser")`
- `classify_area_pure_test` — all paths under `tests/` → `Some("test")`
- `classify_area_mixed_returns_none` — mixed parser + cli → `None`
- `classify_area_pure_docs` — all `.md` → `Some("docs")`
- `risk_low_boundary` — impact_size=5 → `low`
- `risk_medium_boundary` — impact_size=6 → `medium`; impact_size=30 → `medium`
- `risk_high_boundary` — impact_size=31 → `high`
- `cross_pr_conflict_disjoint_sets` — A∩B=∅ → no conflict reported
- `cross_pr_conflict_overlap` — A∩B={"foo"} → conflict with overlap_symbols=["foo"]
- `cross_pr_conflict_missing_cache` — other_pr has no cached impact → reports conflict (conservative)

### Integration test

Golden test fixture under `crates/ecp-cli/tests/fixtures/pr_analyze/`:
- Synthetic git repo with two pre-fabricated PR branches and an indexed ecp graph
- Run `ecp dev pr-analyze --baseline main --pr-head feat/a --pr-number 100 --format json`
- Compare stdout to `expected.json`

### Smoke

After landing, manually open a trivial test PR on this repo and verify:
1. `ecp-pr-analyze.yml` runs
2. Expected labels appear on PR
3. Commit status `ecp/cross-pr-conflict` appears
4. Mergify (once installed) reads them correctly and routes to the right queue

## Migration / rollout

Three-phase rollout, each landable independently:

1. **Phase 1 — `ecp dev pr-analyze` subcommand only** (no workflow, no Mergify):
   - Land the Rust subcommand + unit tests
   - Run manually from a worktree on real PRs to verify classification quality
   - Tune thresholds if obviously wrong
   - PR-able as its own change

2. **Phase 2 — workflow** (no Mergify yet):
   - Add `.github/workflows/ecp-pr-analyze.yml`
   - PRs start getting `ecp:*` labels + status — observe correctness for ~1 week
   - Labels are advisory, no automation consumes them yet
   - PR-able as its own change

3. **Phase 3 — Mergify**:
   - Install Mergify GitHub App on the repo
   - Add `.mergify.yml`
   - Use the `merge-queue` label as opt-in; existing manual `gh pr merge --squash` flow remains valid for non-labeled PRs
   - PR-able as its own change

This sequencing means the ecp side ships in two PRs (subcommand, workflow) before the third-party dependency (Mergify) is involved, keeping the blast radius of each step small.

## Out of scope (explicit YAGNI)

- Selective test runner driven by `ecp impact` — orthogonal; if desired, a separate `ecp dev affected-tests` subcommand + workflow can be added later without touching this design.
- Cross-repo / monorepo coordination.
- Risk-threshold auto-tuning from historical merge data — manual revisit after 1 month is fine.
- Slack / email notifications — Mergify's native handling is sufficient.
- Bisect, per-PR retry budget, custom auto-rebase, custom merge strategies — Mergify provides all.
- Custom queue dashboard / observability — Mergify provides.
- Multi-base branch support — only `main` here.
- Review-approval gates — repo currently has `required_approving_review_count: 0`.

## Future extensions (noted, not now)

- **Speculative pre-merge analysis** — extend `pr-analyze` to score the *post-squash* impact set (not just changed-symbol overlap), tightening the cross-PR conflict signal further.
- **Risk-threshold tuning** — store historical "labeled risk vs. actual CI outcome" stats in a `metrics/` artifact; auto-tune the thresholds quarterly.
- **Per-test impact mapping** — when `ecp impact` returns affected test symbols, surface them on the PR as a comment ("these 3 tests are relevant"); enables a selective test runner downstream.
- **Trunk.io flaky-test integration** — if test flakiness becomes a real cost, layer Trunk on top of Mergify; complementary signal, doesn't conflict with this design.

## References

- Existing `ecp impact` CLI: `crates/ecp-cli/src/commands/impact.rs` (rich `--baseline` mode already implemented)
- Existing `ecp dev` namespace: `crates/ecp-cli/src/commands/dev/`
- Branch protection / Mergify capability investigation: conversation 2026-05-23
- Mergify alternatives analysis (Aviator / Trunk / Kodiak / GitHub native): conversation 2026-05-23
