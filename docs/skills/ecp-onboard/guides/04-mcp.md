# Phase 04 — Agent integration

Goal: collect how the user wants their AI agent wired to ecp. There are
two paths — recommend the richer one per host:

- **Native** (preferred where the host has one — Claude Code, Codex CLI,
  Gemini CLI): `ecp admin <host> install …` wires hooks + a workflow
  skill, not just tool access. Richer signal than MCP alone.
- **MCP** (cross-agent fallback): any host that only speaks MCP (Cursor,
  Zed, Continue.dev, Windsurf, Cline, …) registers the ecp MCP server.

**Do not apply here.** Record MCP picks into `config_inventory.mcp_targets`
and native picks into `config_inventory.native_targets`.

## Step 1: Detect installed hosts

**Do not re-run probes.** Phase 01 already ran the bundled probe and
stashed the result in `config_inventory.system_probe`. Read the host
booleans off it directly:

```
ides.claude_code      → config_inventory.system_probe.ides.claude_code
ides.cursor           → config_inventory.system_probe.ides.cursor
ides.zed              → config_inventory.system_probe.ides.zed
ides.vscode_continue  → config_inventory.system_probe.ides.vscode_continue
```

The probe does not detect Codex CLI / Gemini CLI — if the user names
either, treat it as a native host (table below). If the snapshot is
missing (resume edge-case), re-run the **full** probe from
`_shared/refs/env-detect.md` and re-stash — never `test -d` one at a time.

## Step 2: Map host → path

| Detected / stated host | Path | Apply (Phase 05) |
|---|---|---|
| Claude Code | native | recommend `ecp admin claude install hooks` + `… install skills all` (MCP optional: `… install mcp-server`) |
| Codex CLI | native | recommend `ecp admin codex install skills all` |
| Gemini CLI | native | recommend `ecp admin gemini install native-skill` (or `… install mcp-server`) |
| Cursor / Zed / Continue.dev / Windsurf / Cline | mcp | write MCP config file |
| `persona.ide_pref = unknown` | per-host | native for any detected native host, MCP for detected MCP hosts; let the user opt out |

Native picks are surfaced as concrete next-step commands in Phase 05 (the
user runs them, or accepts the wizard running them) — they are **not**
auto-written like MCP configs. For **multiple detected hosts**, wire each
on its best path; one ecp MCP server can serve several MCP clients at once.

## Step 3: Present menu

```
[Phase: agent integration / Step 4 of 5]

Detected hosts: {list}.

  ✓ Recommended:
     - {native hosts} → native: hooks + skill   via `ecp admin <host> install`
     - {mcp hosts}    → MCP server               (config file)
     Why: {reason}

  Alternative A: only {persona.ide_pref}
  Alternative B: MCP-only everywhere (skip native hooks/skills)
  Alternative C: skip integration (wire later with `ecp admin`)

Reply: accept / a / b / c / skip
```

Wait for user choice.

## Step 4: Record choice

```yaml
native_targets:
  - host: claude-code
    commands:
      - ecp admin claude install hooks
      - ecp admin claude install skills all
    status: queued
mcp_targets:
  - host: cursor
    config_path: ~/.cursor/mcp.json
    status: queued
  # ... one entry per chosen host, on its path
```

## Step 5: Confirm explicit write consent

Per Directive 5 in SKILL.md, the wizard MUST NOT write to user files
outside `~/.ecp/onboarding-summary.md` without consent. Native installs
go through `ecp admin <host>` (ecp owns those writes); MCP installs write
the config files below. Show the user exactly what Phase 05 will do:

```
I'll apply these in Phase 05:
  - Claude Code → run: ecp admin claude install hooks; install skills all
  - Cursor      → write: ~/.cursor/mcp.json

Reply: yes / no / show-content
```

If `show-content`, display the exact `ecp admin` commands (native targets)
and the MCP JSON below (mcp targets), then re-ask.

### MCP config template (mcp-method targets)

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

For hosts with a different schema (e.g. Continue.dev's
`~/.continue/config.json` mixes `models` + `mcpServers`), look up the
exact format in the host's docs at apply time — do not guess.

## Step 6: Advance to Phase 05

Jump to `guides/05-summary.md`.
