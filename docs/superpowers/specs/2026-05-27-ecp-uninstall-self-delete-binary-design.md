# `ecp uninstall` — self-delete binary + `--host` → `--agent` rename

**Date**: 2026-05-27
**Scope**: one PR, three parts (binary self-delete, arg rename, README)

## Problem

`ecp uninstall` reverses every setup side-effect — host integrations (Claude /
Codex / Gemini hooks, MCP, skills), the per-repo git hook, and the `~/.ecp`
cache — but leaves the `ecp` binary itself on disk. A user who runs
`ecp uninstall` expecting "remove ecp" still finds the executable at
`~/.local/bin/ecp` (or wherever they installed it) and must delete it by hand.
The uninstall is incomplete.

Separately, the `--host` flag names the AI coding environment to scope removal
to (`--host claude`). "host" is internal domain vocabulary (`host_integration`
module); as a user-facing flag it is opaque. Rename to `--agent`.

## Goals

1. `ecp uninstall` (no scope flag) deletes the running binary as its final step.
2. Rename `--host` → `--agent` across `uninstall.rs`, including help text,
   doc-comments, internal identifiers, and the error message.
3. README: top-of-page quick-install line + an `### Uninstall` section that
   leads with `ecp uninstall`.

## Non-goals

- No new escape-hatch flag (e.g. `--keep-binary`). Uninstall means uninstall.
- Do **not** rename the `host_integration` module or `admin/host_integration/`
  directory — "host" is consistent domain vocabulary there; renaming it is a
  cross-file refactor outside this scope.
- Do not touch `--keep-cache` semantics beyond confirming binary deletion is
  gated the same as cache wipe.

## Design

### Binary self-delete

A new final step `self-binary`, added in `run()` after the `ecp-cache` wipe,
gated identically: `host_filter.is_none()`. Scoped uninstalls (`--agent claude`)
never delete the binary — the user still wants `ecp`.

Path source: `std::env::current_exe()` — the binary actually executing, robust
to how it was installed (installer script / cargo / npm / PyPI), rather than
reconstructing the installer's `ECP_INSTALL_DIR` logic. The delete core is
factored into a path-taking `remove_self_binary_at(&Path)` (mirroring the
existing `remove_git_hook_at` split) so the step fetches `current_exe()` once
and a test can drive the core against a tmpdir path.

Platform behaviour:

| Platform | Mechanism | Summary status |
|---|---|---|
| Linux / macOS | `std::fs::remove_file(current_exe())` — Unix permits unlinking a running executable (inode survives until the process exits) | `self-binary  done` |
| Windows | spawn a detached `cmd /c timeout /t 3 /nobreak >nul 2>&1 & del /f /q "<exe>"`; the main process returns normally; the file lock releases on exit; the delayed `del` then succeeds | `self-binary  scheduled (deletes after exit)` |

The Windows process is spawned with `DETACHED_PROCESS | CREATE_NO_WINDOW` so it
survives the parent's exit and shows no console window. The parent does **not**
wait on it.

`--dry-run`: print `[dry-run] would remove binary: <path>`, take no action,
record a dry-run summary entry like every other step.

### `--host` → `--agent` rename (within `uninstall.rs` only)

- arg field `host` → `agent`; `#[arg(long)]` becomes `--agent`
- help text: "Only uninstall integration for one coding agent
  (claude, codex, gemini). Omit to uninstall all detected agents."
- `host_filter` → `agent_filter`; `matches_host` → `matches_agent`;
  `validate_host_filter` → `validate_agent_filter`
- error message: `unknown agent '{other}' — expected claude, codex, or gemini`
- module doc-comment lines 11-12 updated to say "agent" where they currently
  say "--host"
- inline comments at the git-hook and cache-wipe gates updated to "no --agent
  filter"

### README

1. Below the i18n line, inside the centered `<div>`, a one-line quick-install:
   a `curl … install.sh | sh` block commented `# Linux / macOS`, followed by
   `[All install options](#-install) · [Uninstall](#uninstall)`.
2. New `### Uninstall` section in the Install chapter leading with
   `ecp uninstall` (one command removes integrations + `~/.ecp` + the binary),
   noting `--dry-run` to preview and `--agent <name>` to scope to one agent.
   Package-manager removals (`npm uninstall -g` / `uv tool uninstall` /
   `cargo uninstall`) listed for users who installed that way.

## Error handling

- `remove_file` failure (Unix): recorded as `self-binary  ERROR (<msg>)` in the
  summary; does not abort — it is already the last step.
- `current_exe()` failure: recorded as `ERROR`, skip the delete.
- Windows spawn failure: recorded as `ERROR`. A successful spawn is reported as
  `scheduled` — the actual `del` outcome is not observable from the exited
  parent, and this is stated honestly rather than claimed as `done`.

## Testing

- Unix: a test that copies a dummy executable into a tmpdir, points the
  delete-self logic at that path (factor the path-taking core out of
  `current_exe()` so a test can pass an arbitrary path, mirroring the existing
  `remove_git_hook_at` split), and asserts the file is gone after the call.
- Windows: assert the command string is assembled correctly (contains the exe
  path, `del /f /q`, the timeout) without actually spawning — the delayed
  deletion is not unit-testable in-process. Gate with `#[cfg(windows)]`.
- `--dry-run`: assert the binary still exists and the summary entry is the
  dry-run variant.
- Scoped uninstall (`--agent claude`): assert the binary is **not** touched.
- Rename: assert `--agent` parses and `--host` is rejected (clap error), and
  `unknown agent` error text for a bad value.
