---
title: "[nightly] Windows CI broken"
labels: ["ci", "ci-windows-broken"]
---

The Windows nightly job failed. This issue is **auto-updated** on every
subsequent failure; same title → same issue, no duplicates.

- **Failed run**: {{ env.RUN_URL }}
- **Commit**: `{{ env.RUN_SHA }}`
- **Trigger**: `{{ env.RUN_EVENT }}`

## Triage steps

1. Open the failed run and check whether it's a runner-infrastructure
   flake (e.g. `actions/partner-runner-images#169` bash startup race).
   If yes — re-run via the workflow's "Re-run failed jobs" button; close
   this issue when next nightly is green.
2. If the failure is real, identify the offending commit range:
   ```bash
   # locally:
   LAST_GREEN=$(gh run list --workflow=ci-windows-nightly.yml --status success --limit 1 --json headSha -q '.[0].headSha')
   git log --oneline "$LAST_GREEN..{{ env.RUN_SHA }}"
   ```
3. Reproduce locally (if you have a Windows box) or via a temp branch
   with `workflow_dispatch`:
   ```bash
   gh workflow run ci-windows-nightly.yml --ref <suspect-sha>
   ```
4. Fix in a follow-up PR; the merge will re-run the nightly on
   `push: branches: [main]` and close this issue's red status.

## Why this surfaced as an issue and not a blocking PR check

Windows is **not a per-PR required check** for this repo — see
`.github/workflows/ci.yml` matrix and `.github/workflows/ci-windows-nightly.yml`
for the rationale. Detection window is 24h (next scheduled run) rather
than per-PR; the explicit failure tracking via this issue is the
escape hatch so silent breakage doesn't accumulate.
