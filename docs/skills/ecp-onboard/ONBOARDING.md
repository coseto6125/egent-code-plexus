
# ecp-onboard

## Start now (no permission required)

On SKILL load, your VERY NEXT actions — **no preamble, no permission-seeking, no
summarizing back**:

1. Check whether `~/.ecp/onboarding-summary.md` exists (Directive 6 — resume).
2. If not resuming: read `_shared/refs/env-detect.md` and run its **single bundled
   probe script** as ONE Bash call. Stash the JSON in
   `config_inventory.system_probe`. Never call `command -v` / `test -d` one at a time.
3. Derive the persona per `_shared/refs/persona-inference.md`.
4. Read `guides/01-install.md` and emit its 3-choice menu.

The jump table tells you which file to fetch next — follow it.

## Directives (non-negotiable)

1. **Recommend → user picks accept / change / skip.** Every choice point. Never
   auto-decide.
2. **Only use already-loaded prompts + probes listed in
   `_shared/refs/env-detect.md`.** Don't fish for user files beyond your context.
3. **Never silently retry, never silently switch methods.** On failure: show stderr
   verbatim → consult the common-cause table → offer retry / change-method / skip.
4. **Never block on the install download.** When Phase 01 starts a background
   download, advance to Phase 02 to collect later choices. Apply them as a batch at
   the T6 gate, after the binary is verified.
5. **Background = `ecp` CLI only.** Every applied action goes through `ecp`. Never
   write user files outside `~/.ecp/onboarding-summary.md` (plus Phase 04 writes the
   user approved — IDE MCP configs and/or native `ecp admin <host> install` runs).
6. **New session start:** if `~/.ecp/onboarding-summary.md` exists, read it first and
   offer resume / redo-phase / start-over.

## Persona inference (summary)

Apply `_shared/refs/persona-inference.md`'s rule table top-down at each phase start:

- `lang_pref` — conversation language
- `install_pref` — cargo-binstall / brew / tarball
- `scope_pref` — `single-repo` vs `group-heavy`
- `ide_pref` — host to wire, native vs MCP

Dimension still `unknown` → use the `(empty)` row default; ask when a phase needs it.

## Jump table

Walk phases in order. **Load each guide ONLY when entering that phase.** Don't
pre-fetch later guides — touching `guides/0X` before `guides/0X-1` finalizes wastes
tokens.

| Intent / state | Next guide |
|---|---|
| Fresh session, no prior summary | guides/01-install.md |
| Install done, no `~/.ecp/registry.json` yet | guides/02-first-index.md |
| Indexed but no group registered | guides/03-group.md (skip if `scope_pref = single-repo`) |
| Indexed + grouped, agent not yet integrated | guides/04-mcp.md |
| All previous phases complete | guides/05-summary.md |
| Resuming an interrupted session | Read summary, ask user which phase to resume |

## Ordering rules

- **Phases 01–04 are choice-collection only.** Each guide records its decision into
  in-memory `config_inventory`. No `ecp` apply commands in 02/03/04.
- **Phase 05 is the apply-and-summarize gate.** Wait for the Phase 01 download +
  verify `ecp --version`, then drain `config_inventory` into one batch of `ecp admin`
  calls in order: index → group → agent integration (MCP configs for `mcp_targets`;
  native `ecp admin <host> install` for `native_targets`). Verify each succeeds
  before the next.
- **If Phase 01 install failed**, do not run Phase 05's apply step. Re-enter Phase 01
  with failure context from the common-cause table.

## CLI flag lookups

For exact `ecp <cmd>` flag syntax, read `_shared/cli/<cmd>.md`. If missing/outdated,
run `ecp <cmd> --help` live as ground truth — never invent flags.

## Hard "don't" list

- Do not silently retry a failed command.
- Do not switch install methods without user consent.
- Do not modify `~/.zshrc`, `~/.gitconfig`, or any user file not explicitly listed
  under Phase 04 (IDE MCP configs / native `ecp admin <host> install` targets).
- Do not assume future ecp versions have a flag — verify against the CLI reference
  cards or live `--help`.



<!-- guide: 01-install -->

# Phase 01 — Install

Goal: produce a verified `ecp` binary on PATH. Start the install in the
background and advance to Phase 02 without waiting.

## Step 1: Probe the system (single call)

Run the **bundled probe script** in `_shared/refs/env-detect.md` —
paste the whole `bash <<'PROBE' … PROBE` block into ONE Bash tool
call. It runs all probes concurrently and emits one JSON object in
~100ms (vs ~10s if you call `command -v` one tool at a time).

Stash the result:

```
config_inventory.system_probe = <parsed JSON>
```

All downstream phases (02 / 03 / 04 / 05) re-use `config_inventory.system_probe`.
**Do not re-run `command -v` / `test -d` individually anywhere in the wizard.**
If the user installs something mid-wizard, re-run the whole probe to
refresh the snapshot.

## Step 2: Apply persona × probe → recommendation

Read fields off `config_inventory.system_probe`:

| persona.install_pref | probe fields | Recommendation |
|---|---|---|
| `cargo-binstall` | `installers.cargo_binstall = true` | `cargo binstall egent-code-plexus` |
| `cargo-binstall` | `installers.cargo_binstall = false`, `installers.cargo = true` | `cargo install egent-code-plexus` (slower; source build) + suggest installing cargo-binstall next time |
| `brew` | `installers.brew = true` | `brew install <tap>/egent-code-plexus` (substitute the actual tap name from the README) |
| `github-release-tarball` (or fallback) | `installers.curl = true` | `curl -L https://github.com/<owner>/egent-code-plexus/releases/latest/download/ecp-<target>.tar.gz \| tar -xz -C ~/bin/` |
| (ecp already installed) | `ecp.installed = true` | Verification only; skip download. Use `ecp.version` to display "Detected ecp 0.2.0". |

## Step 3: Present 3-choice menu

Format (translate to `lang_pref`):

```
[Phase: install / Step 1 of 5]

Based on your persona ({install_pref}, {os}-{arch}), recommendation:

  ✓ Recommended: {recommended_command}
     Why: {reason}

  Alternative A: {alt_a_command}
     Why: {reason_a}

  Alternative B: {alt_b_command}
     Why: {reason_b}

  Skip: I've already installed it (I'll jump to verification)

Reply: accept / a / b / skip
```

Wait for user choice.

## Step 4: Start background install

If choice ≠ skip:

- Spawn the chosen command in the background (use the agent's
  `run_in_background` shell execution mode).
- Do NOT wait for completion. Record the background task ID into
  `config_inventory.install_task_id`.
- Immediately tell the user: "Install running in background. Continuing
  to Phase 02 — your binary will be verified before any `ecp` commands
  are executed."

If choice == skip:

- Run `ecp --version` synchronously and record the output. If it fails,
  loop back to Step 3.

## Step 5: Advance to Phase 02 (do NOT block on install)

Jump to `guides/02-first-index.md`. The Phase 01 background install
keeps running while later phases collect their choices.

## Failure handling

If the install command fails (whether discovered at T6 verification or
earlier), do not auto-retry. Consult the **install** rows in the
common-cause table in `_shared/refs/env-detect.md` and offer the user:

- **Retry** the same command (verbatim)
- **Change method** — re-present the 3-choice menu, excluding the failed option
- **Skip** — mark `config_inventory.install_status = failed` and let
  Phase 05 surface the failure in the final summary

Never silently switch methods.


<!-- guide: 02-first-index -->

# Phase 02 — First-index

Goal: collect the user's choice of which repo(s) to index. **Do not run
`ecp admin index` here** — only record the choice into
`config_inventory.first_index`.

## Step 1: Detect candidate repos

The agent should NOT scan the filesystem broadly. Instead, infer candidates
from already-loaded context:

- Current working directory (if the chat is happening inside a repo)
- Any repo path the user mentioned in chat
- The repo containing this SKILL pack itself (if recipient is reading
  the file by absolute path)

If no candidate is obvious, ask the user directly: "Which repository
should I index first?"

## Step 2: Apply persona → recommendation

| persona.scope_pref | Recommendation |
|---|---|
| `group-heavy` | Index 2–3 sibling repos in a single batch (user lists them) |
| `single-repo` | Index the current repo only |
| `unknown` | Ask the user; default to "current directory" |

## Step 3: Present 3-choice menu

```
[Phase: first-index / Step 2 of 5]

Based on your persona ({scope_pref}), recommendation:

  ✓ Recommended: index {recommended_repo_list}
     Why: {reason}

  Alternative A: index only the current directory
  Alternative B: skip indexing for now (you can run `ecp admin index` later)

Reply: accept / a / b / skip
```

Wait for user choice.

## Step 4: Record choice (DO NOT execute)

Record into `config_inventory.first_index`:

```yaml
first_index:
  repos: [<chosen list>]
  status: queued     # NOT 'done' — apply happens in Phase 05
```

## Step 5: Advance to Phase 03

Jump to `guides/03-group.md`. If `persona.scope_pref = single-repo` AND
only one repo was selected, **skip directly to** `guides/04-mcp.md`
(no group needed).


<!-- guide: 03-group -->

# Phase 03 — Group

Goal: collect group definitions if the user has multiple repos. **Do not
run `ecp admin group add` here** — record into `config_inventory.groups`.

This phase is **skipped** when:

- `persona.scope_pref = single-repo` AND `first_index.repos` has length 1
- The user explicitly skipped Phase 02

## Step 1: Detect grouping signals

- Were multiple repos selected in Phase 02?
- Do their paths share a common parent (suggests a monorepo / workspace)?
- Did the chat mention "team", "monorepo", "service mesh", or similar?

If none of these → ask the user: "Do you have related repos you'd like
to query as a unit (e.g., a frontend + backend pair, or a microservices
suite)?"

## Step 2: Apply persona → group layout recommendation

| Pattern | Recommendation |
|---|---|
| 2–3 repos sharing parent dir | One group named after the parent dir |
| Frontend + backend mentioned | Two groups (`frontend`, `backend`), each with the relevant repo |
| User-named group | Take the user's name verbatim |

## Step 3: Present 3-choice menu

```
[Phase: group / Step 3 of 5]

Detected grouping signals: {summary}.

  ✓ Recommended: create group "{recommended_name}" with repos {repo_list}
     Why: {reason}

  Alternative A: separate groups per pair (e.g., A, B)
  Alternative B: no groups (you can `ecp admin group add` later)

Reply: accept / a / b / skip
```

Wait for user choice.

## Step 4: Record choice

```yaml
groups:
  - name: {chosen_name}
    repos: [{chosen_repos}]
    status: queued
```

## Step 5: Advance to Phase 04

Jump to `guides/04-mcp.md`.


<!-- guide: 04-mcp -->

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

Per Directive 5 in SKILL.md, the wizard writes only to
`~/.ecp/onboarding-summary.md` until the user consents. Native installs
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


<!-- guide: 05-summary -->

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

This path is for hosts ecp cannot script (Cursor, Zed, VS Code,
Continue.dev). A host that appears under `native_targets` has an
installer — `ecp admin <host> install mcp-server` — so route it through
4a and leave it out of this step. Step 4a's rule holds here too: never
hand-edit a config ecp owns.

For each remaining MCP target:

- **Idempotency:** if the config file already exists, **merge** the
  `ecp` entry into the existing `mcpServers` object rather than
  overwriting the file. Use `jq` for JSON files.
- **Backup:** before any write, copy the existing file to
  `<path>.bak.<timestamp>`.

```bash
# Example: Cursor
target=~/.cursor/mcp.json
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
