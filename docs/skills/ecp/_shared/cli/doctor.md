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
| `skill:<name>` | installed copy matches repo source | stale / not installed → Warn |
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
- **skills**: `ecp admin claude install skills <name>`.
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
changes. `--dry-run` prints that diff without writing — the same engine
`doctor`'s skill check uses to detect staleness.

## Related: install diff

`ecp admin claude install skills <target>` now always prints a diff of what it
changes (added / removed / modified files, with a warning when an installed
file looks hand-edited). `--dry-run` prints that diff without writing —
the same engine `doctor` uses to detect skill staleness.
