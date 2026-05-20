# SKILL Smoke Playbook (T5)

Manual end-to-end test. Run **before each release** that touches anything
under `docs/skills/ecp-onboard/`. Not run in CI (cross-platform install
matrix is out of scope).

## Setup

1. Fresh sandbox: a VM, container, or remote machine where:
   - `ecp` is NOT installed
   - `~/.ecp/` does NOT exist
   - The recipient's editor of choice is installed (Claude Code,
     Cursor, etc.)

## Test cases

### Case A: Cross-agent URL bootstrap (any LLM)

1. Paste into a fresh chat session of the target agent (Cursor / Aider / Gemini CLI / etc.):
   > "Fetch https://raw.githubusercontent.com/<owner>/egent-code-plexus/main/docs/skills/ecp-onboard/SKILL.md and follow it as my onboarding wizard for egent-code-plexus."
2. **Expect:** agent reads SKILL.md, runs probes, emits Phase 01 3-choice menu.
3. Pick `accept`.
4. **Expect:** download starts in background; agent advances to Phase 02 immediately (does not wait).
5. Answer Phase 02–04 prompts.
6. **Expect:** Phase 05 waits for download to verify before running `ecp admin index`.
7. **Verify:**
   - `which ecp` returns a path
   - `~/.ecp/registry.json` exists
   - `~/.ecp/onboarding-summary.md` exists
   - IDE MCP config file written (for the IDE chosen)
   - `ecp find . --repo <indexed-repo>` returns results

### Case B: ShareOnboardingGuide (Claude Code)

1. In Claude Code, from `docs/skills/ecp-onboard/` cwd:
   - Run the `ShareOnboardingGuide` tool with mode `check`.
2. **Expect:** short-code link returned.
3. Open that link in a fresh Claude Code session (different machine or `claude --reset`).
4. Repeat cases A.2 onward.

### Case C: Resume after interruption

1. Run Case A; at Phase 03 say `quit` or close terminal.
2. **Expect:** `~/.ecp/onboarding-summary.md` has frontmatter with
   `last_phase_completed: 02-first-index`.
3. Start a new agent session, paste URL bootstrap.
4. **Expect:** agent reads summary, offers "Resume from Phase 03? Redo a specific phase? Start over?"
5. Pick "Resume" — confirm Phase 03 starts correctly.

### Case D: Install failure path

1. Sabotage: in the test VM, place a `cargo-binstall` shim that exits 1.
2. Run Case A and pick `cargo binstall`.
3. **Expect:** Phase 05's T6 gate detects the failure, surfaces stderr,
   consults common-cause table, offers retry / change-method / skip.

## Pass criteria

- All 4 cases complete the listed "verify" steps.
- No file outside `~/.ecp/onboarding-summary.md` and the IDE MCP configs is modified.
- No silent retries observed.
- Persona inference picks the correct branch based on the test agent's CLAUDE.md (or equivalent).

## When to update this playbook

- A new phase is added → add corresponding verify steps.
- A new distribution outlet is added → new Case.
- A regression is found in production → pin it with a new Case before fixing.
