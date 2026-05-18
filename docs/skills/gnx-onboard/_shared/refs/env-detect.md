# Environment Detection

Shared snippets used by Phase 01 (install) and Phase 04 (mcp). The agent
runs these probes via its existing shell-execution tool — they are
plain `command -v` / `uname` invocations that produce no side effects.

## Probes

### OS + architecture

```bash
uname -sm
# → "Darwin arm64"  / "Linux x86_64"  / "Linux aarch64"
```

### Package managers (one line each, exit 0 = present)

```bash
command -v cargo
command -v cargo-binstall
command -v brew
command -v curl
command -v wget
```

### IDEs (configuration paths exist?)

```bash
# Claude Code
test -d "$HOME/.claude"

# Cursor (macOS / Linux)
test -d "$HOME/Library/Application Support/Cursor" || test -d "$HOME/.config/Cursor"

# Zed
test -d "$HOME/.config/zed"

# VS Code (with Continue.dev plugin convention)
test -d "$HOME/.vscode" || test -d "$HOME/.continue"
```

### Existing gnx state

```bash
command -v gnx && gnx --version
test -d "$HOME/.gnx"
test -f "$HOME/.gnx/registry.json"
```

## Common-cause table

When a phase's apply step fails, the agent maps the symptom to one of the
hypotheses below before offering retry / change-method / skip.

| Phase | Symptom | Hypotheses (priority order) |
|---|---|---|
| install | `cargo binstall` not found | (1) `cargo` not installed; (2) `cargo-binstall` subcommand missing — suggest `cargo install cargo-binstall` |
| install | binstall fails to fetch tarball | (1) no prebuilt for this target triple → fallback to source build (`cargo install graph-nexus`); (2) network / proxy; (3) GitHub release not yet propagated |
| install | `brew install` fails with "no such formula" | tap not added — suggest `brew tap <author>/gnx` |
| install | `curl` of GitHub release returns 404 | version tag mismatch — confirm latest release tag on `gh release list` |
| first-index | `gnx admin index ... → not a git repo` | wrong path / no `.git` directory at repo root |
| first-index | index runs >3 min | large repo / vendored deps not ignored — recommend a `.gnxignore` |
| first-index | `permission denied` writing to `~/.gnx` | recipient's HOME not writable (rare; container env) — suggest `GNX_HOME=$PWD/.gnx` env override |
| group | `gnx admin group add ... → repo not in registry` | repo not yet indexed — re-run phase 02 for that path first |
| mcp | IDE config written but IDE doesn't pick up new tool | (1) IDE not restarted; (2) wrong config path (Cursor has two: `~/.cursor/mcp.json` and per-project `.cursor/mcp.json`); (3) IDE version too old |

## When probes fail

If a probe itself errors (e.g., `uname` not available, `command -v` returns
non-zero unexpectedly), switch to **manual mode**: ask the user directly
for OS / installed package managers, mark `system_probe = manual` in the
persona, and stop attempting silent detection for the rest of the session.
