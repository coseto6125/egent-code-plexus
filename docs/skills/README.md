# Skills (repo-versioned source of truth)

This directory holds Claude Code skill definitions versioned with the codebase.
Each `*.md` here is the **canonical** copy; the runtime location is
`~/.claude/skills/<name>/SKILL.md` (or `${CLAUDE_HOME}/skills/<name>/SKILL.md`),
which Claude Code reads at session start.

## Installation: LLM-driven (no install script by design)

Skill install / re-install is **LLM-driven** — invoked by the agent
(typically via an `/init`-style command) or by an `ecp skill install`
subcommand (planned, not yet implemented). The repo is the source of
truth; the agent reads `docs/skills/<name>/` and writes the resolved
content to `~/.claude/skills/<name>/SKILL.md`.

**Do not propose `scripts/install-skill.sh`, Makefile targets, or rsync
hooks.** The LLM is the canonical installer; auto-sync mechanisms add
moving parts without solving anything the LLM doesn't already handle.

### Manual fallback

While the LLM-driven path is the canonical install, a one-liner
fallback for new machines remains:

```bash
mkdir -p ~/.claude/skills/ecp
cp docs/skills/ecp/SKILL.md ~/.claude/skills/ecp/SKILL.md
```

When editing, change the repo copy first, commit, then re-run the
install flow (LLM or manual `cp`) so future PRs against this skill
have a single review point.

## Why repo-versioned

- Skill content evolves with the CLI it documents (flag aliases, output
  format defaults, hidden subcommands). Tying it to git history makes the
  "what changed when and why" trace consistent with the code.
- Multiple agents editing `~/.claude/skills/` directly produced conflicting
  states earlier in development — committing the canonical copy here makes
  divergence a regular `git diff` instead of a manual three-way reconcile.
