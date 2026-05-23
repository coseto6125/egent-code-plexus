# Phase 04 — MCP

Goal: collect the user's choice of which IDE(s) to wire the ecp MCP
server into. **Do not write the MCP config files here** — record into
`config_inventory.mcp_targets`.

## Step 1: Detect installed IDEs

**Do not re-run probes.** Phase 01 already ran the bundled probe and
stashed the result in `config_inventory.system_probe`. Read the IDE
booleans off it directly:

```
ides.claude_code      → config_inventory.system_probe.ides.claude_code
ides.cursor           → config_inventory.system_probe.ides.cursor
ides.zed              → config_inventory.system_probe.ides.zed
ides.vscode_continue  → config_inventory.system_probe.ides.vscode_continue
```

If for some reason the snapshot is missing (resume edge-case), re-run
the **full** probe from `_shared/refs/env-detect.md` and re-stash —
never call `test -d` individually.

## Step 2: Apply persona → recommendation

| persona.ide_pref | Recommendation |
|---|---|
| `claude-code` | Write Claude Code MCP config |
| `cursor` | Write Cursor MCP config |
| `zed` | Write Zed MCP config |
| `vscode` | Write Continue.dev config |
| `unknown` | Recommend all IDEs that the probe detected; let user opt out |

For **multiple detected IDEs**, recommend wiring all of them (an MCP
server can serve multiple clients simultaneously).

## Step 3: Present menu

```
[Phase: mcp / Step 4 of 5]

Detected IDEs: {list of detected IDEs}.

  ✓ Recommended: wire MCP into {ide_list}
     Why: {reason}

  Alternative A: only {persona.ide_pref}
  Alternative B: skip MCP setup (you can `ecp admin mcp` later)

Reply: accept / a / b / skip
```

Wait for user choice.

## Step 4: Record choice

Tag each chosen IDE as `scripted` (driven by `ecp admin <agent>
install`) or `paste-snippet` (no scripted installer; user pastes the
snippet). Phase 05 runs the scripted ones and prints snippets for the
rest.

```yaml
mcp_targets:
  - ide: claude-code
    mode: scripted
    command: ecp admin claude install mcp-server
    status: queued
  - ide: codex
    mode: scripted
    command: ecp admin codex install skills
    status: queued
  - ide: gemini
    mode: scripted
    command: ecp admin gemini install skills
    status: queued
  - ide: cursor
    mode: paste-snippet
    snippet_target: ~/.cursor/mcp.json
    status: queued
  - ide: zed
    mode: paste-snippet
    snippet_target: ~/.config/zed/settings.json
    status: queued
  # ... one entry per chosen IDE
```

Resolve the exact `command` via `ecp admin <agent> install --help` at
apply time — components evolve between versions.

## Step 5: Confirm before apply

Per Directive 5 in SKILL.md, the wizard never edits IDE config files
directly. Restate the Phase 05 plan and wait for confirmation:

```
Phase 05 will run:
  - ecp admin claude install mcp-server   (Claude Code)
  - ecp admin codex install skills        (Codex)
  - ecp admin gemini install skills       (Gemini)

Then print paste snippets for:
  - ~/.cursor/mcp.json            (Cursor)
  - ~/.config/zed/settings.json   (Zed)

The only file the wizard writes itself is
~/.ecp/onboarding-summary.md.

Reply: yes / no / show-snippet
```

If `show-snippet`, display the snippet for the requested IDE (template
below), then re-ask.

### Paste snippet for unscripted IDEs

Standard MCP-servers schema (Cursor, most generic clients):

```json
{
  "mcpServers": {
    "ecp": {
      "command": "ecp",
      "args": ["admin", "mcp", "serve"]
    }
  }
}
```

For IDEs that use a different schema (e.g., Continue.dev's
`~/.continue/config.json` mixes `models` + `mcpServers`, Zed has its
own `context_servers` block), look up the exact format in the IDE's
docs at apply time rather than guessing.

## Step 6: Advance to Phase 05

Jump to `guides/05-summary.md`.
