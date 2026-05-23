# Skills (repo-versioned source of truth)

This directory holds Claude Code skill definitions versioned with the codebase.
Each `*.md` here is the **canonical** copy; the runtime location is
`~/.claude/skills/<name>/SKILL.md` (or `${CLAUDE_HOME}/skills/<name>/SKILL.md`),
which Claude Code reads at session start.

## Installation: LLM-driven or `ecp admin claude install skills`

Skill install / re-install is **LLM-driven by default** — invoked by the
agent (typically via an `/init`-style command). The repo is the source
of truth; the agent reads `docs/skills/<name>/` and writes the resolved
content to `~/.claude/skills/<name>/SKILL.md`.

For machine-driven re-sync (CI, new dev machine, after a schema bump),
the bundled CLI subcommand copies the canonical `docs/skills/ecp/` tree
into `~/.claude/skills/ecp/`:

```bash
ecp admin claude install skills ecp        # just the ecp skill
ecp admin claude install skills simplify   # just simplify
ecp admin claude install skills all        # both
```

The CLI sources `Ecp` from `docs/skills/ecp/` (this dir) and `Simplify`
from `skill_sample/claude/simplify/`. Existing global files are
overwritten — re-run after pulling main to pick up schema additions.

**Do not propose `scripts/install-skill.sh`, Makefile targets, or rsync
hooks.** The CLI subcommand is the canonical machine-driven path;
auto-sync at the filesystem level adds moving parts without solving
anything the CLI doesn't already handle.

### Manual fallback

If neither the LLM nor the CLI path is available, copy by hand:

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
