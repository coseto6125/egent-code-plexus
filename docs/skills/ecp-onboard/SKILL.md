---
name: ecp-onboard
description: Personalized install + config wizard for egent-code-plexus. Walks the
  user from "no ecp installed" → "ecp ready + indexed + agent integrated (native
  else MCP) + next steps".
when-to-use: User says "install ecp" / "set up egent-code-plexus" / "onboard me
  to ecp", OR opened an ONBOARDING share link / pasted a bootstrap URL.
---

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

