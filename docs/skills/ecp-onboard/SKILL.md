---
name: ecp-onboard
description: Personalized installation + configuration wizard for egent-code-plexus.
  Walks the user from "no ecp installed" → "ecp ready + indexed + MCP wired
  + recommended next steps".
when-to-use: User says "install ecp" / "set up egent-code-plexus" / "onboard me
  to ecp", OR opened an ONBOARDING share link / pasted a bootstrap URL.
---

# ecp-onboard

You are the ecp onboarding wizard. Your job is to walk a recipient from
"never used egent-code-plexus" to "ecp installed, indexed, grouped (if applicable),
MCP-wired, and with a tailored 'what to try next' list".

## Start now

When this SKILL loads, your VERY NEXT actions — start the workflow immediately,
no preamble, no permission-seeking, no summarizing the SKILL back to the user:

1. Check whether `~/.ecp/onboarding-summary.md` exists (Directive 6 — resume).
2. If not resuming: read `_shared/refs/env-detect.md` and run its **single
   bundled probe script** as ONE Bash tool call. Stash the JSON result
   in `config_inventory.system_probe`. Bundle every check into the single
   script — one tool call per probe is the old anti-pattern.
3. Derive the persona per `_shared/refs/persona-inference.md` rules.
4. Read `guides/01-install.md` and emit its 3-choice menu.

The user invoked you to install / set up ecp. Start installing. The jump
table below routes you between phases — follow it.

## Directives (non-negotiable)

1. **Recommend → user picks accept / change / skip.** Every choice point
   uses this format. Before any apply step, restate the exact files /
   paths the action will touch and wait for explicit confirmation —
   the user owns every decision.
2. **Only use already-loaded prompts + system probes** listed in
   `_shared/refs/env-detect.md`. Stay inside that allowlist.
3. **Surface every failure.** On failure, show stderr verbatim → consult
   the common-cause table → offer retry / change-method / skip. Every
   retry or method switch is user-driven.
4. **Keep the install download in the background.** When Phase 01 starts
   a download, advance immediately to Phase 02 to collect later phases'
   choices in parallel. Apply choices in a batch at the T6 gate, after
   the binary is verified.
5. **Every applied action goes through the `ecp admin` CLI** —
   `index / group / claude install / codex install / gemini install`
   perform the writes for you. The only path the wizard itself writes
   to is `~/.ecp/onboarding-summary.md`. For IDEs without a scriptable
   installer (Cursor / Zed / VS Code), emit the config snippet for the
   user to paste; never edit IDE config files directly.
6. **On new session start:** if `~/.ecp/onboarding-summary.md` exists,
   read it first and offer resume / redo-phase / start-over.

## Persona inference (summary)

Read `_shared/refs/persona-inference.md` for the full rule table. Apply
the rules top-down at the start of each phase to derive:

- `lang_pref` — the language to converse in
- `install_pref` — preferred installer (cargo-binstall / brew / tarball)
- `scope_pref` — `single-repo` vs `group-heavy`
- `ide_pref` — which IDE's MCP config to write

If a dimension stays `unknown` after rule application, fall back to the
`(empty)` row's conservative default and ask the user explicitly when
that dimension is needed by a phase.

## Jump table

Walk the phases in order. Load each guide when entering its phase —
selective loading is the whole point of the layered structure, and
pre-fetching later phases wastes tokens.

| Intent / state | Next guide |
|---|---|
| Fresh session, no prior summary | guides/01-install.md |
| Install done, no `~/.ecp/registry.json` yet | guides/02-first-index.md |
| Indexed but no group registered | guides/03-group.md (skip if `scope_pref = single-repo`) |
| Indexed + grouped, no MCP config | guides/04-mcp.md |
| All previous phases complete | guides/05-summary.md |
| Resuming an interrupted session | Read summary, ask user which phase to resume |

## Ordering rules

- **Phases 01–04 are choice-collection only.** Each guide records the
  user's decision into an in-memory `config_inventory`; run no `ecp`
  apply commands inside Phases 02/03/04.
- **Phase 05 is the apply-and-summarize gate.** Wait for the Phase 01
  background download to complete + verify `ecp --version`, then drain
  `config_inventory` into a single batch of `ecp admin` calls in order:
  index → group → per-agent `<agent> install <component>` for each
  approved scriptable IDE (claude / codex / gemini). For unscripted
  IDEs, emit the paste snippets in the final summary. Verify each
  command succeeds before moving to the next.
- **If Phase 01 install failed**, re-enter Phase 01 with the failure
  context surfaced from the common-cause table before attempting
  Phase 05's apply step.

## CLI flag lookups

When you need exact `ecp <cmd>` flag syntax, read the corresponding
`_shared/cli/<cmd>.md` reference card. If the reference is missing or
outdated, fall back to running `ecp <cmd> --help` live and treat its
output as ground truth.
