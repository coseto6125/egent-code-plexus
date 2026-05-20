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

Use `_shared/cli/admin-index.md` for exact flag syntax. If the
reference card is missing or outdated, fall back to `ecp admin index
--help` and treat its output as ground truth.

On success, mark `status: done` in the inventory. On failure, follow
the common-cause table → retry / change-method / skip.

## Step 3: Apply groups

For each group in `config_inventory.groups`:

```bash
ecp admin group add --repo <repo_path> <group_name>
```

(See `_shared/cli/admin-group.md` for the exact subcommand
shape — `add` vs `create` etc.)

## Step 4: Apply IDE integrations

For each target in `config_inventory.mcp_targets` (user already
consented in Phase 04 Step 5):

- **Scripted IDEs (`mode: scripted`):** run the recorded `command`.
  `ecp admin <agent> install` handles idempotency, backup, and
  config-merging for you — the wizard does not touch the IDE config
  file directly.

  ```bash
  # claude / codex / gemini
  $target.command
  # e.g., ecp admin claude install mcp-server
  ```

  On success, mark `status: done` in the inventory. On failure,
  surface stderr → consult the common-cause table → offer
  retry / skip.

- **Paste-snippet IDEs (`mode: paste-snippet`):** emit the snippet to
  the user with its target path; the user pastes manually. Standard
  `mcpServers` schema (Cursor, generic clients):

  ```text
  Paste into ~/.cursor/mcp.json (merge into the existing "mcpServers"
  object if the file is non-empty):

      {
        "mcpServers": {
          "ecp": {
            "command": "ecp",
            "args": ["admin", "mcp", "serve"]
          }
        }
      }
  ```

  Mark `status: snippet-emitted` in the inventory once the snippet has
  been shown. IDEs with a different schema (Zed `context_servers`,
  Continue.dev mixed config) — look up the exact shape in the IDE's
  docs at apply time rather than guessing.

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

## Phase 04 mcp
- [x] ran `ecp admin claude install mcp-server`
- [x] ran `ecp admin gemini install skills`
- [x] emitted paste snippet for ~/.cursor/mcp.json (Cursor)

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
MCP wired into: {list}
Summary saved to: ~/.ecp/onboarding-summary.md

Try next:
- {recommendation 1}
- {recommendation 2}
- {recommendation 3}

Re-run `ecp admin coverage` anytime to see graph health.
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
