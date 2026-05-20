# Skills (repo-versioned source of truth)

This directory holds Claude Code skill definitions versioned with the codebase.
Each `*.md` here is the **canonical** copy; the runtime location is
`~/.claude/skills/<name>/SKILL.md` (or `${CLAUDE_HOME}/skills/<name>/SKILL.md`),
which Claude Code reads at session start.

## Manual sync

There is no symlink or auto-sync hook by design — installing the skill on a
new machine is a one-liner:

```bash
mkdir -p ~/.claude/skills/ecp
cp docs/skills/ecp.md ~/.claude/skills/ecp/SKILL.md
```

When editing, change the repo copy first, commit, then propagate to
`~/.claude/skills/` so future PRs against this skill have a single review
point.

## Why repo-versioned

- Skill content evolves with the CLI it documents (flag aliases, output
  format defaults, hidden subcommands). Tying it to git history makes the
  "what changed when and why" trace consistent with the code.
- Multiple agents editing `~/.claude/skills/` directly produced conflicting
  states earlier in development — committing the canonical copy here makes
  divergence a regular `git diff` instead of a manual three-way reconcile.
