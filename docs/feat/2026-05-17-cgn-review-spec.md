# `cgn review` — LLM-Workflow Audit Aggregator

**Status**: spec draft 2026-05-17
**Owner**: this worktree (`scan-llm-hallucination-fixes`)

## Problem

Individual cgn primitives (`scan`, `impact`, `shape-check`, `coverage`,
`diff`) each surface one slice of signal. The LLM consumer has to
remember to call each, parse each independently, and de-duplicate
findings. In practice the LLM rarely invokes most of them; the ones
they do invoke (`scan` especially) emit too much noise to be acted on
without further filtering.

Outcome today: code-edit → LLM ships → reviewer finds the issue cgn
*could* have caught had it been a one-shot aggregator with sane
defaults.

## Goal

One command, one report, high-signal-only:

```
cgn review                       # default: git diff HEAD
cgn review --since main          # since branch divergence
cgn review --files crates/foo/*  # explicit list
```

Output: per-file findings from all relevant primitives, **filtered to
high-confidence signals only**, in a compact format suited to landing
in an LLM hook's response budget.

## Non-goals

- Not a replacement for language-native linters (`cargo check`, `ruff`,
  `tsc`). Those run first; `cgn review` adds the graph-aware semantic
  layer they can't see.
- Not a full code review. No prose, no style suggestions, no
  refactoring advice.
- Not stateful — no persistent issue tracking, no comparison against
  prior reviews. Each invocation is a fresh snapshot.

## Input

| Flag | Default | Behavior |
|---|---|---|
| `--since <ref>` | `HEAD` | `git diff <ref>...HEAD --name-only` selects files |
| `--files <glob>` | unset | Explicit list overrides `--since` |
| `--repo <path>` | cwd | Standard cgn repo selector |
| `--format` | `toon` | `toon` / `json` |

No input → defaults to changed files in working tree (`git diff HEAD
--name-only` plus untracked-but-staged).

## Composition

Each constituent runs **in warning mode** (high-confidence only). The
aggregator suppresses findings below the confidence threshold so the
report stays signal-dense.

| Constituent (library only, NO standalone CLI/MCP) | Filter | What it contributes |
|---|---|---|
| `impact` | `risk_level >= medium` | "this symbol has 4+ callers; review blast radius" |
| `egress` (was `tool-map`) | new HTTP/DB/Redis/queue calls | "this PR adds aiohttp call to `api.example.com`" |
| `shape-check` (folded in) | drift detected | "route `POST /api/users` response gained field `created_at`" |
| `coverage` (BlindSpot subset) | per-file blind spots | "framework `flask` not in graph — extern calls unverified" |
| `diff` (resolver, folded in) | binding tier-degradation | "import `foo::Bar` silently re-resolved to extern" |

Initial scope: **impact + coverage BlindSpot + egress diff** (i.e., new
external calls introduced by the change set). `shape-check` and
`diff` (resolver) land in a follow-up — they need cross-file context
the MVP doesn't yet assemble.

**Scan is NOT a constituent** — symbol-typo detection has been
removed entirely (signal:noise ≈ 1:10 even with filters; language-
native linters + compilers cover the actionable subset). See the
pivot rationale in the worktree's commit history.

## Output schema

```toon
files[N]{path,findings}:
  crates/foo/bar.rs[2]:
    {kind: typo,    severity: warn, line: 42, message: "did you mean 'query_order'?", source: scan}
    {kind: impact,  severity: info, line: 18, message: "8 callers", source: impact}
  crates/foo/baz.rs[1]:
    {kind: blind_spot, severity: info, line: 0, message: "framework 'flask' not in graph", source: coverage}

summary:
  files_reviewed: 2
  warn_count: 1
  info_count: 2
  clean_files: 0
  elapsed_ms: 320
```

If all files clean:
```
status: clean
files_reviewed: 5
elapsed_ms: 180
```

## Implementation sketch

```
crates/cgn-cli/src/commands/review/
├── mod.rs           — CLI args + top-level dispatch + output emit
├── scope.rs         — resolve --since / --files / cwd into file list
├── aggregate.rs     — per-file: call constituents, collect findings, dedupe
└── findings.rs      — Finding type + severity ordering + format
```

Constituents are called as library functions (not subprocesses).
`scan::run`, `impact::run`, etc. already return structured payloads —
we extract findings from those rather than text-parse stdout.

`scan` needs a new mode flag (`--confidence lev1`) for warning-mode
output. Done as a small follow-up to scan in this worktree before the
aggregator lands; see Task #6.

## Why this pivot is right

This spec replaces the alternative of fanning out the Python
import-aware extractor to 13 more languages (~3000 LOC estimate). That
fan-out fixes one symptom (extern noise) at one layer (scan-only). The
aggregator fixes the *workflow* — LLM doesn't need to remember any
single primitive, and noise is filtered at composition time rather than
per-primitive.

The Python POC import-aware code stays in the tree as a reference
implementation. Other languages adopt the same pattern only if measured
need shows scan's per-language signal is the bottleneck — which the
review aggregator's high-confidence filter likely makes moot.

## Open questions

- Should `cgn review` auto-trigger language-native linters too, or
  strictly stay in graph-semantic territory? Default for MVP: stay in
  cgn territory. Hook composition handles the linter side.
- Cross-file findings (e.g., "you removed a public symbol that another
  file still imports") — these need impact's data joined across the
  changed-file set. Punt to follow-up.
- PostToolUse hook integration — separate task once the command is
  stable. Schema is hook-friendly (small, structured) by design.
