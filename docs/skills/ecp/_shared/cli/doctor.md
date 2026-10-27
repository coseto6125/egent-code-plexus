# ecp admin doctor

Environment health check. Aggregates independent checks into one report so a
drifted setup (stale skills, stale graph, outdated host config) surfaces in a
single command instead of failing silently mid-workflow.

## Usage

```bash
ecp admin doctor                  # all checks, read-only report
ecp admin doctor --fix            # all checks + fix everything fixable
ecp admin doctor registry         # run only the registry check
ecp admin doctor registry --fix   # run only registry + fix it (single-target fix)
ecp admin doctor --format json    # structured output for CI / tooling
```

`[check]` is one of: `skills` `index` `host` `config` `registry` `version`.
Omit it to run every check.

## Checks

| Check | Pass | Warn / Fail |
|---|---|---|
| `skill:<name>` | installed copy matches the copy this ecp ships | not installed / outdated / locally edited / no install record → Warn |
| `index` | graph fresher than working tree | stale → Warn; missing → Fail |
| `host:<tool>` | integrated, or optional and absent | config outdated → Warn |
| `config:git` | git on PATH | absent → **Fail** (core features need it) |
| `config:ecp-home` / `config:claude-dir` | exists / writable | missing / read-only → Warn |
| `registry:*` | no orphans / corruption | orphan dirs, missing graph/meta, corrupt meta → Warn |
| `version` | local == latest tag (via `git ls-remote`) | newer tag available → Warn; offline → Warn |

Exit code is non-zero when any check is **Fail**, so CI can gate on `ecp admin doctor`.
Warnings alone do not fail the run.

## `--fix`

Reruns the remediation for the selected check(s) in place:
- **skills**: installs the copy this ecp binary ships, only for a skill that
  is missing or that nobody edited since ecp installed it. Each Claude skill
  install writes `.ecp-install.json`, the hash of every file it wrote, into
  the skill dir. A skill whose files differ from that record, or that has no
  record, stays report-only: review it with `--dry-run` and install it by
  hand. A skill that already matches the shipped copy reports Pass, and
  `--fix` (re)writes its record.

  The doctor check, `--fix` and `ecp admin claude install skills` all use the
  copy built into the binary.
- **index**: `ecp admin index --repo .`.
- **registry**: removes orphan index dirs (missing/corrupt graph & meta stay
  report-only — a rebuild, not a delete, is the safe fix).
- **host**: reinstalls *scripted* hosts (claude / gemini mcp+native, codex mcp).
  Interactive-only or stub hosts stay report-only.

`config` and `version` are always report-only — `--fix` never rewrites
user-owned host configs, deletes user data, or triggers a multi-minute rebuild
of the binary.

## Related: install diff

`ecp admin claude install skills <target>` always prints a diff of what it
changes: added, removed and modified files. `--dry-run` prints that diff
without writing.
