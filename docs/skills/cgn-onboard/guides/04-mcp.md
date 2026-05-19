# Phase 04 — MCP

Goal: collect the user's choice of which IDE(s) to wire the cgn MCP
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
  Alternative B: skip MCP setup (you can `cgn admin mcp` later)

Reply: accept / a / b / skip
```

Wait for user choice.

## Step 4: Record choice

```yaml
mcp_targets:
  - ide: claude-code
    config_path: ~/.claude/.mcp.json  # or the per-project equivalent
    status: queued
  - ide: cursor
    config_path: ~/.cursor/mcp.json
    status: queued
  # ... one entry per chosen IDE
```

## Step 5: Confirm explicit write consent

Per Directive 5 in SKILL.md, the wizard MUST NOT write to user files
outside `~/.cgn/onboarding-summary.md` without consent. Show the user
the exact paths the wizard will write to in Phase 05, and ask:

```
I'll write these files in Phase 05:
  - ~/.claude/.mcp.json   (Claude Code)
  - ~/.cursor/mcp.json    (Cursor)

Reply: yes / no / show-content
```

If `show-content`, display the JSON the wizard would write (template
below), then re-ask.

### MCP config template

```json
{
  "mcpServers": {
    "cgn": {
      "command": "cgn",
      "args": ["admin", "mcp", "serve"]
    }
  }
}
```

For IDEs that use a different schema (e.g., Continue.dev uses
`~/.continue/config.json` with a `models` + `mcpServers` mix), look up
the exact format in the IDE's docs at apply time — do not guess.

## Step 6: Advance to Phase 05

Jump to `guides/05-summary.md`.
