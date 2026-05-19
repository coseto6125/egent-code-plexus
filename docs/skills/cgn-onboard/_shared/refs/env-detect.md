# Environment Detection

Single bundled probe — replaces N×Bash-tool round-trips with **one call**
emitting structured JSON. The agent runs this script ONCE at Phase 01
Step 1 and re-uses the result for the rest of the wizard.

## Privacy & safety guarantees

This script is intentionally minimal. The agent must NOT modify it or
add probes beyond this list before showing the script body to the user.

| Guarantee | How |
|---|---|
| **Read-only** | No writes outside `$(mktemp -d)`; no network calls; no `eval` |
| **No path leakage** | Output contains booleans (`true` / `false`) for tool presence — never the absolute path that `command -v` returned |
| **No file content read** | `test -f` / `test -d` only — never opens / pipes file contents |
| **No env-var inspection** | Only `$HOME` (for path construction); not exported, not in output |
| **No PID / hostname / username / git config** | Skipped on purpose |
| **Public version string only** | `cgn --version` reveals only the binary's own self-reported version, which is the same as any public release tag |
| **Auto-cleanup** | `trap ... EXIT` removes the tmpdir |

The output JSON contains ONLY these fields:
`os` · `arch` · `installers.{cargo,cargo_binstall,brew,curl,wget}` (bool) ·
`ides.{claude_code,cursor,zed,vscode_continue}` (bool) ·
`cgn.{installed,version,registry_exists}`.

## The probe script

Paste **this whole block** into a single Bash tool call. Independent
probes run in parallel via background subshells (`&` + `wait`); the
final `jq` assembles the JSON from per-probe tmp files.

```bash
bash <<'PROBE'
set -u
umask 077

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# --- run all probes concurrently ---
( uname -s > "$tmp/os" ) &
( uname -m > "$tmp/arch" ) &

for tool in cargo cargo-binstall brew curl wget cgn; do
    ( command -v "$tool" >/dev/null 2>&1 && echo true || echo false ) > "$tmp/has_$tool" &
done

( command -v cgn >/dev/null 2>&1 && cgn --version 2>/dev/null | awk '{print $NF}' || echo "" ) > "$tmp/cgn_ver" &

( test -d "$HOME/.claude" && echo true || echo false )                           > "$tmp/ide_claude_code" &
( ( test -d "$HOME/Library/Application Support/Cursor" || test -d "$HOME/.config/Cursor" ) \
        && echo true || echo false )                                              > "$tmp/ide_cursor" &
( test -d "$HOME/.config/zed" && echo true || echo false )                       > "$tmp/ide_zed" &
( ( test -d "$HOME/.vscode" || test -d "$HOME/.continue" ) \
        && echo true || echo false )                                              > "$tmp/ide_vscode_continue" &

( test -f "$HOME/.cgn/registry.json" && echo true || echo false )                > "$tmp/cgn_registry" &

wait

# --- assemble JSON (no paths, only booleans + public version) ---
jq -n \
  --rawfile os         "$tmp/os" \
  --rawfile arch       "$tmp/arch" \
  --rawfile cgn_ver    "$tmp/cgn_ver" \
  --argjson c_cargo    "$(cat "$tmp/has_cargo")" \
  --argjson c_binstall "$(cat "$tmp/has_cargo-binstall")" \
  --argjson c_brew     "$(cat "$tmp/has_brew")" \
  --argjson c_curl     "$(cat "$tmp/has_curl")" \
  --argjson c_wget     "$(cat "$tmp/has_wget")" \
  --argjson c_cgn      "$(cat "$tmp/has_cgn")" \
  --argjson i_claude   "$(cat "$tmp/ide_claude_code")" \
  --argjson i_cursor   "$(cat "$tmp/ide_cursor")" \
  --argjson i_zed      "$(cat "$tmp/ide_zed")" \
  --argjson i_vscode   "$(cat "$tmp/ide_vscode_continue")" \
  --argjson g_reg      "$(cat "$tmp/cgn_registry")" \
  '{
     os:    ($os   | rtrimstr("\n")),
     arch:  ($arch | rtrimstr("\n")),
     installers: {
       cargo:           $c_cargo,
       cargo_binstall:  $c_binstall,
       brew:            $c_brew,
       curl:            $c_curl,
       wget:            $c_wget
     },
     ides: {
       claude_code:      $i_claude,
       cursor:           $i_cursor,
       zed:              $i_zed,
       vscode_continue:  $i_vscode
     },
     cgn: {
       installed:        $c_cgn,
       version:          ($cgn_ver | rtrimstr("\n")),
       registry_exists:  $g_reg
     }
   }'
PROBE
```

### Reading the result

Parse the JSON once and stash into the in-memory `config_inventory`:

```
config_inventory.system_probe = <parsed json>
```

All downstream phases read from this object — **do not re-run individual
`command -v` / `test -d` calls**. If the user changes something
(installs cargo-binstall mid-wizard), re-run the WHOLE probe to refresh
the snapshot; never patch the JSON manually.

## Common-cause table

When a phase's apply step fails, the agent maps the symptom to one of the
hypotheses below before offering retry / change-method / skip.

| Phase | Symptom | Hypotheses (priority order) |
|---|---|---|
| install | `cargo binstall` not found | (1) `cargo` not installed; (2) `cargo-binstall` subcommand missing — suggest `cargo install cargo-binstall` |
| install | binstall fails to fetch tarball | (1) no prebuilt for this target triple → fallback to source build (`cargo install code-graph-nexus`); (2) network / proxy; (3) GitHub release not yet propagated |
| install | `brew install` fails with "no such formula" | tap not added — suggest `brew tap <author>/cgn` |
| install | `curl` of GitHub release returns 404 | version tag mismatch — confirm latest release tag on `gh release list` |
| first-index | `cgn admin index ... → not a git repo` | wrong path / no `.git` directory at repo root |
| first-index | index runs >3 min | large repo / vendored deps not ignored — recommend a `.cgnignore` |
| first-index | `permission denied` writing to `~/.cgn` | recipient's HOME not writable (rare; container env) — suggest `CGN_HOME=$PWD/.cgn` env override |
| group | `cgn admin group add ... → repo not in registry` | repo not yet indexed — re-run phase 02 for that path first |
| mcp | IDE config written but IDE doesn't pick up new tool | (1) IDE not restarted; (2) wrong config path (Cursor has two: `~/.cursor/mcp.json` and per-project `.cursor/mcp.json`); (3) IDE version too old |

## When probes fail

If a probe itself errors (e.g., `uname` not available, `command -v` returns
non-zero unexpectedly), switch to **manual mode**: ask the user directly
for OS / installed package managers, mark `system_probe = manual` in the
persona, and stop attempting silent detection for the rest of the session.
