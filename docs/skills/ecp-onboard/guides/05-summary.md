# Phase 05 — Apply + Summary

Goal: at the T6 gate, wait for the background install (Phase 01) to
finish + verify `ecp --version`, then drain `config_inventory` into a
single batch of `ecp admin` calls. Finally, persist the summary and
emit the recommendation list.

## Step 1: T6 gate — wait for install

```bash
# Wait for the background task started in Phase 01.
# Use the agent's mechanism (e.g., poll the task_id until status = done).
ecp --version
```

If `ecp --version` fails:

- Surface stderr to the user.
- Consult `_shared/refs/env-detect.md` common-cause table.
- Re-enter Phase 01's failure-handling branch.
- DO NOT proceed to Step 2 until install is verified.

If `ecp --version` succeeds, parse the version and stash it as
`config_inventory.installed_version`.

## Step 2: Apply first-index

For each repo in `config_inventory.first_index.repos`:

```bash
ecp admin index --repo <repo_path>
```

Use `_shared/cli/<version>/admin-index.md` for exact flag syntax. If
the version is missing, fall back to `ecp admin index --help`.

On success, mark `status: done` in the inventory. On failure, follow
the common-cause table → retry / change-method / skip.

## Step 3: Apply groups

For each group in `config_inventory.groups`:

```bash
ecp admin group add --repo <repo_path> <group_name>
```

(See `_shared/cli/admin-group.md` for the exact subcommand
shape — `add` vs `create` etc.)

## Step 4: Apply agent integration

User consented in Phase 04 Step 5. Apply native targets first, then MCP
targets.

### 4a — Native targets (`config_inventory.native_targets`)

For each native host, run its recorded `ecp admin <host> install`
commands verbatim (ecp owns these writes — hooks land in the host's
settings, skills copy into the host's skills dir):

```bash
# Example: Claude Code
ecp admin claude install hooks
ecp admin claude install skills all
ecp admin claude status        # confirm INSTALLED
```

> **Guidance import**: `ecp admin claude install skills` also copies an
> `ECP.md` guidance file into `~/.claude/` and appends `@ECP.md` to the
> global `~/.claude/CLAUDE.md`. This loads every session so the agent
> defaults to `ecp` for structural queries instead of falling back to
> grep. Pass `--no-claude-md` to skip the import.

On failure, show stderr → common-cause table → retry / skip. These are
`ecp admin` calls, not raw file writes — never hand-edit the host's
settings to emulate them.

### 4b — MCP targets (`config_inventory.mcp_targets`)

For each MCP target:

- **Idempotency:** if the config file already exists, **merge** the
  `ecp` entry into the existing `mcpServers` object rather than
  overwriting the file. Use `jq` for JSON files.
- **Backup:** before any write, copy the existing file to
  `<path>.bak.<timestamp>`.

```bash
# Example: Claude Code
target=~/.claude/.mcp.json
if [[ -f "$target" ]]; then
    cp "$target" "$target.bak.$(date +%s)"
    jq '.mcpServers.ecp = {"command":"ecp","args":["admin","mcp","serve"]}' \
        "$target" > "$target.tmp" && mv "$target.tmp" "$target"
else
    mkdir -p "$(dirname "$target")"
    cat > "$target" <<'JSON'
{ "mcpServers": { "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] } } }
JSON
fi
```

## Step 5: Persist summary

Write `~/.ecp/onboarding-summary.md`:

```markdown
---
wizard_version: 0.2.0
last_phase_completed: 05-summary
installed_version: {version}
persona_snapshot:
  lang_pref: {lang}
  install_pref: {install}
  scope_pref: {scope}
  ide_pref: {ide}
generated_at: {ISO 8601 timestamp}
---

## Phase 01 install
- [x] command run: {command}
- [x] verified: ecp --version → {version}

## Phase 02 first-index
- [x] indexed: {list of repos}

## Phase 03 group
- [x] group "{name}" created with repos: {list}
(or)
- [ ] skipped — single-repo workflow

## Phase 04 agent integration
- [x] native: Claude Code — hooks + skills (`ecp admin claude install`)
- [x] guidance: @ECP.md import added to ~/.claude/CLAUDE.md
- [x] mcp: wrote ~/.cursor/mcp.json (Cursor)

## Phase 05 summary
- [x] this file
```

Each step from the inventory becomes a `- [x]` or `- [ ] skipped — <reason>`
line. The YAML frontmatter is machine-readable for future resume sessions.

## Step 6: Emit recommendations

Open `_shared/refs/recommendation-templates.md`. Pick 3–5 lines that
match the persona (see the file's own header for the selection rule).
Format as a final chat message:

```
🎉 Onboarding complete.

Indexed: {list}
Groups: {list or "none"}
Agent integration: {native hosts: hooks+skill} · {mcp hosts: MCP server}
Summary saved to: ~/.ecp/onboarding-summary.md

Try next:
- {recommendation 1}
- {recommendation 2}
- {recommendation 3}

Re-run `ecp summary --repo @all --detailed` anytime to see graph health.
```

The wizard's job ends here.

## Resume case

If `~/.ecp/onboarding-summary.md` already exists at session start
(per SKILL.md directive 6), read its frontmatter. If
`last_phase_completed = 05-summary`, the user already finished —
greet them with the recommendation list only. Otherwise offer:

```
Last session got to Phase {N}. What would you like to do?
- Resume from Phase {N+1}
- Redo a specific phase (which?)
- Start over (this will overwrite the summary)
```
