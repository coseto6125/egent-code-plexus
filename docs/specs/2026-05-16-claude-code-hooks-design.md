# Claude Code hooks for Rust cgn — design

**Status**: draft (awaiting approval)
**Date**: 2026-05-16
**Author**: ported design from `~/bin/cgn.branch-spike/claude-hooks/gitnexus-hook.cjs`

## §1 Motivation

The legacy cgn (npm `gitnexus`) ships a Claude Code hook
(`~/.claude/hooks/gitnexus/gitnexus-hook.cjs`) that gives the agent
graph-aware context at four Claude Code hook points. PR #11 replaced the
runtime `~/bin/cgn` binary with the Rust cgn, but the hook still calls
the legacy `gitnexus` CLI. We need a hook surface that talks to the new
binary and exploits its in-process advantages.

Rather than port the cjs verbatim, we wire the hook through a new
hidden subcommand `cgn hook <event> --claude-code` so the hook runs
**in-process** — same Rust binary that's already loaded — and reads the
graph directly via the existing `engine::Engine` mmap path instead of
spawning a second `cgn search` subprocess per fire.

Performance budget (measured on enor's machine, 20-run median after 3
warmup, see brainstorm Q1 transcript):
- `bash -c exit` cold: 0.8 ms
- `cgn --version` cold: 1.5 ms
- `cgn mcp tools` (clap-tree walk) cold: 1.6 ms
- `node -e exit` cold: 9.4 ms
- `node + spawn cgn subprocess` cold: 13.4 ms

The Rust binary is ~6× faster cold-start than Node and saves the entire
second subprocess hop on PreToolUse (which fires 10-20× per session).
For PreToolUse alone we save ~500 ms-1 s cumulative wall time per session.

## §2 CLI surface

### §2.1 Hook entry point

```
cgn hook <event> --claude-code
```

Hidden (`#[command(hide = true)]` like existing `hook-handle` /
`hook-watcher`). Reads JSON envelope on stdin (the shape Claude Code
posts), writes JSON response to stdout (the
`{"hookSpecificOutput": {"hookEventName", "additionalContext"}}` shape
Claude Code consumes).

Events:
- `user-prompt-submit` — surface async reindex completion / failure
- `pre-tool-use`       — graph augmentation for Grep / Glob / Bash
- `post-tool-use`      — auto-reindex after git mutations
- `session-start`      — render rules template + worktree detection

`--claude-code` is a flag, not the positional, so future agent hosts add
their own flag (e.g. `--codex`, `--gemini`) without colliding with event
names. Exactly one host flag must be set; absence is an error.

### §2.2 Admin subcommands

Add to `commands::admin`:

| Subcommand | Behaviour |
|---|---|
| `cgn admin install-hook --claude-code [--events <csv>]` | Install hook entries in `~/.claude/settings.json`. With `--events`, only those events. Without, falls through to TUI multi-select. Idempotent (re-running same events doesn't duplicate). Also copies bundled `rules.md` to `~/.claude/hooks/cgn/rules.md` on first install. |
| `cgn admin status --claude-code` | Report which events are currently installed (parse settings.json), which are missing, and the resolved path each entry points at. |
| `cgn admin uninstall-hook --claude-code [--events <csv>]` | Remove hook entries. With `--events`, only those; without, all 4. |
| (TUI route) `cgn admin` → "Claude Code hooks" → multi-select checkbox | Same as `install-hook` with interactive event picker. Surfaces current state inline. |

settings.json mutation rules:
- Read existing file → parse JSON → merge (don't overwrite unrelated entries from other tools, e.g. the existing `gitnexus` hook from the legacy install).
- Atomic write via temp + rename so a kill-during-write doesn't corrupt.
- Per-event entry shape mirrors the legacy snippet structure (matcher / hooks array / timeout / statusMessage).

## §3 Event handler details

### §3.1 SessionStart

Mirror of legacy `renderRules()` + `detectWorktreeNeedingIndex()`.

Template lookup order:
1. `<repoRoot>/.claude/cgn-rules.md` — per-project override
2. `~/.claude/hooks/cgn/rules.md` — global default (shipped from the
   bundled `crates/cgn-cli/assets/claude-code/rules.md`)

Placeholders rendered:
- `{{stats.nodes}}` / `{{stats.edges}}` — read from rkyv-archived
  `ArchivedZeroCopyGraph` in `.cgn/graph.bin` (in-process via
  `engine::Engine::load(...).graph()`)
- `{{head}}` — short SHA from `git rev-parse HEAD` (subprocess; no graph
  schema dependency)
- `{{#if graphify}}…{{/if}}` — conditional on `graphify-out/` existing
- `{{#if wiki}}…{{/if}}` — conditional on
  `graphify-out/wiki/index.md` existing

Worktree-needs-index detection: when cwd's git toplevel is a worktree
(`.git` is a file, not a dir) and `.cgn/` is missing, emit a
hint suggesting `cgn admin index` (or whatever the per-worktree
indexing command is in the new tooling — referenced in the rules
template, not by literal string here, so future renames don't bit-rot
the hook).

### §3.2 UserPromptSubmit

Mirror of legacy `handleUserPromptSubmit()`.

Marker files live in `.cgn/`:
- `.rebuild-complete` — written by PostToolUse spawn on success
- `.rebuild-failed`   — written by PostToolUse spawn on terminal failure
- `last-rebuild.log`  — accumulated stdout/stderr of the spawn

Behaviour:
1. If `.rebuild-failed` exists → read last 3 lines of `last-rebuild.log`,
   surface as additionalContext, then `unlink(.rebuild-failed)`.
2. Else if `.rebuild-complete` exists → read meta.json for `node_count`
   / `indexed_at`, surface success notice, then `unlink(.rebuild-complete)`.
3. Else → no-op.

Failure takes priority over success (more actionable to the agent).

### §3.3 PreToolUse

Mirror of legacy `handlePreToolUse()` but in-process.

Pattern extraction (port verbatim from cjs):
- `Grep` → `tool_input.pattern`
- `Glob` → first ≥3-char alphanumeric stem in the glob pattern
- `Bash` → strip shell quotes; if `rg` / `grep`, find the first
  non-flag, non-flag-value argument ≥3 chars

If no pattern (or <3 chars) → no-op.

In-process search (key perf win):

1. Resolve graph path from cwd (`graph_path::resolve(...)`).
2. `engine::Engine::load(graph_path)` (~100 µs mmap).
3. Call new helper `commands::search::compute_hits(args, &Engine)
   -> Result<Vec<Hit>, CgnError>` — extracted from existing `run()`. We
   split `run()` into `compute_hits()` + `emit_hits()` so the hook
   can call `compute_hits` directly without going through stdout.
4. Format top-K (cap at 5) hits as:
   `cgn hit: <kind> <file>:<line> <name> [score:<s>]`
5. Emit as `hookSpecificOutput.additionalContext`.

Cap at 5 hits OR 2 KB serialized additionalContext, whichever fires
first, to keep token cost bounded (each fire adds to the agent's
context window).

If `.cgn/graph.bin` is missing → no-op (don't block agent;
the agent will still run Grep). Same for any error path: the hook
must never block tool execution.

### §3.4 PostToolUse

Mirror of legacy `handlePostToolUse()`.

Trigger condition:
- `tool_name == "Bash"` AND
- shell-quote-stripped command matches
  `\bgit\s+(commit|merge|rebase|cherry-pick|pull)(\s|$)` AND
- `tool_output.exit_code == 0` (skip failed commands)

Stale-detection: use existing `auto_ensure::ensure_index(graph_path,
worktree_root) -> EnsureResult`. **No core schema change**:
`BranchMeta` already has `indexed_at: String` (timestamp) but no commit
SHA, and `auto_ensure` is mtime-based, which is in fact more correct
than SHA-based — `git commit --amend` changes SHA without changing
working files (no reindex needed), and uncommitted local edits don't
change SHA but should trigger stale (which mtime detects).

If `EnsureResult::Stale { age_seconds }`:
1. Spawn detached `cgn admin index` under flock at
   `.cgn/.analyze.lock` (port flock pattern from cjs).
2. On terminal success write `.cgn/.rebuild-complete`; on
   final failure (after MAX=3 attempts) write `.rebuild-failed`.
   Both clear the opposite marker.
3. Surface to agent:
   `cgn reindex started in background (stale ~{age_seconds}s)...`

If `EnsureResult::Ready` → no-op (silent).
If `EnsureResult::Missing` → hint that index doesn't exist; agent can
decide to run `cgn admin index`.

Hook itself returns immediately. The spawned analyze runs async.

## §4 Code layout

New files in `crates/cgn-cli/`:

```
src/commands/
  hook.rs                              # NEW — dispatch + 4 event impls
  admin/claude_code.rs                 # NEW — install/uninstall/status
src/main.rs                            # +1 enum variant, +1 dispatch arm
assets/claude-code/
  rules.md                             # NEW — bundled SessionStart template
tests/
  hook_pre_tool_use.rs                 # NEW — pattern extraction + hit shape
  hook_post_tool_use.rs                # NEW — git command detection + flock
  hook_marker_cycle.rs                 # NEW — marker write→read→unlink
  hook_install_settings.rs             # NEW — settings.json merge idempotence
```

Helper to add in `crates/cgn-cli/src/commands/search.rs`:

```rust
pub fn compute_hits(args: SearchArgs, engine: &Engine)
    -> Result<Vec<Hit>, CgnError>;
```

Extract the existing logic in `run()` that builds `hits` (currently
calls `bm25_hits_from_graph` then sort+truncate) and re-shape `run()`
to call `compute_hits` + `emit_hits`. Net diff: ~20 lines of refactor,
no behaviour change for the CLI surface.

## §5 Hook protocol envelope

Claude Code stdin JSON (relevant fields, observed from legacy cjs):

```json
{
  "cwd": "/abs/path",
  "tool_name": "Bash" | "Grep" | "Glob" | ...,
  "tool_input": { ... },
  "tool_output": { "exit_code": 0, ... }
}
```

Response on stdout:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "additionalContext": "...injected text..."
  }
}
```

No response (empty stdout) = no-op, hook is invisible. This is the
default for "no pattern matched", "graph missing", "marker absent", etc.

## §6 Failure modes & graceful degradation

| Scenario | Hook behaviour |
|---|---|
| `.cgn/` doesn't exist | Silent no-op (don't block agent) |
| `graph.bin` corrupt / version mismatch | Silent no-op, log to stderr |
| Detached spawn fails to start | Surface error in PostToolUse response, don't write `.rebuild-failed` (that marker is for the spawn process's exit code, not the launcher) |
| settings.json missing on install | Create with just our entries |
| settings.json corrupt JSON on install | Error out — don't overwrite |
| Race between two PostToolUse fires | flock guards spawn; second fire becomes a no-op |
| flock unavailable (busybox / Windows) | Deferred, see §9 — out of scope for v1 (Linux + macOS) |

## §7 Testing

Per-event integration test crate (`tests/hook_*.rs`):

- **hook_pre_tool_use** — feed synthetic Claude Code stdin envelope,
  run `cgn hook pre-tool-use --claude-code`, assert stdout JSON
  contains expected `additionalContext` for Grep / Glob / Bash patterns
  AND that ≤3 char patterns or missing patterns produce empty output.
- **hook_post_tool_use** — set up a tempfs git repo + `.cgn/`
  with a stale-marker-eligible graph, feed `git commit` envelope,
  assert spawned analyze process exists (e.g. via PID file or
  `pgrep -f`). Test the no-spawn paths: non-git Bash, failed
  exit_code, unchanged HEAD.
- **hook_marker_cycle** — write `.rebuild-complete` directly, feed
  `user-prompt-submit` envelope, assert response surfaces success and
  marker is unlinked.
- **hook_install_settings** — write a `settings.json` with unrelated
  entries (mimicking the legacy `gitnexus` hook), call
  `install-hook --claude-code --events session-start`, assert merge is
  correct, legacy entries preserved, our new entry present.
  Re-run with same events, assert no duplication.

Unit tests inside `commands/hook.rs` (cfg(test)):

- `extract_pattern_*` — pattern extraction parity with cjs (port the
  same cases the cjs test suite covers, if any survive — otherwise
  hand-curate from the cjs regex chains).
- `strip_shell_quotes` — port `stripShellQuotes` (the bytewise quote
  walker, lines 210-238 of cjs).

## §8 Migration / coexistence

The legacy `~/.claude/hooks/gitnexus/gitnexus-hook.cjs` still works
(it calls npm `gitnexus` directly, not `~/bin/cgn`). The new hook
installs to `~/.claude/hooks/cgn/` (different directory) and writes
**new** entries in `settings.json` alongside the legacy ones.

Coexistence behaviour:
- Both hooks fire on every event.
- Old hook writes markers to `.gitnexus/`, new to `.cgn/`.
- Both surface their own `additionalContext` — agent sees both. This
  is acceptable noise; users who want to silence the legacy can
  manually edit `settings.json` (or run `gitnexus uninstall-hook`
  whatever the legacy equivalent is — out of scope here).

Long-term we expect users to drop the legacy hook once they migrate
their workflow to Rust cgn; we don't force that step.

## §9 Out of scope (deferred)

- Windows / busybox flock alternative for PostToolUse — current spec
  is Linux + macOS only. Falls back to no-spawn (silent) on platforms
  without `flock`.
- Embedding-aware reindex (the legacy hook re-runs `analyze
  --embeddings` to preserve semantic search) — `cgn admin index`
  doesn't yet have an embeddings flag in mainline. When it does, port
  the `hadEmbeddings` check from cjs lines 449-462.
- Per-host marker contextualization (showing the agent which Claude
  Code session triggered the reindex). Not needed for v1.
- Auto-uninstall of legacy gitnexus-hook.cjs when our hook installs.
  Explicit user action only.

## §10 Decision trajectory

§10.1 Hook language: chose Rust subcommand `cgn hook` (vs Node cjs vs
bash). Drivers: cold-start 6× faster than Node, no second subprocess
on PreToolUse, ties hook to cgn release cycle (one-shot upgrade).

§10.2 Selective install: chose TUI + CLI flags both (`cgn admin
install-hook` with optional `--events`). TUI for discoverability, CLI
flags for scripting.

§10.3 PreToolUse augmentation source: chose in-process call into
`commands::search::compute_hits` (vs new `cgn augment` subcommand).
Avoids exposing internal helper as user-visible command surface.

§10.4 PostToolUse stale detection: chose existing
`auto_ensure::ensure_index` mtime-based (vs adding `head_commit`
field to `BranchMeta`). Zero core schema change, and mtime is more
correct than SHA for amend / uncommitted-edit cases.

§10.5 settings.json strategy: chose merge (vs overwrite). Coexists
with legacy `gitnexus` hook entries until user manually migrates.

§10.6 Hook CLI shape: chose `cgn hook <event> --<host>` (vs
`cgn hook <host> <event>`). Event is the primary axis; host is a
flag so future hosts add `--codex` / `--gemini` without colliding
with event names.
