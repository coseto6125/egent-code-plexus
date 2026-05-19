---
name: cgn
description: Use for cgn command and workflow reference. Start here for command selection, help routing, and links into the detailed guides.
---

# cgn — Entry Point

This is the **single entry point** for the Codex-facing `cgn` skill set.

When you need to use `cgn`, do not guess from memory. First identify the task category, then open the matching guide.

---

## Layer 1: Core Directives

These rules apply to every `cgn` task.

### Directive 1: Use the actual help output
`cgn --help` is the top-level command map.
`cgn admin --help` is the admin subcommand map.

Do not treat `cgn admin` as a help command; it launches the interactive TUI by default.

### Directive 2: Prefer the smallest command that fits
If a task can be answered by the top-level help or a single subcommand help page, use that before reading any broader reference.

### Directive 3: Keep task-specific workflows separate
Command syntax, review workflows, and broader repository guidance should live in separate guides instead of one large doc.

---

## Layer 2: Decision Tree

| If you need... | Open guide |
|---|---|
| Command names, flags, output formats, or admin subcommands | [`guides/command-reference.md`](./guides/command-reference.md) |
| Change review workflow for changed files | [`../simplify/SKILL.md`](../simplify/SKILL.md) |

> If you are unsure which command to use, start with `cgn --help`, then open the matching guide.

---

## Layer 3: On-Demand References

These are support files, not entry points.

- `guides/` — detailed command and workflow references
- `../simplify/SKILL.md` — graph-aware review workflow built on top of `cgn`

If you find yourself reading every file under `skill_sample/codex/` for one task, you skipped Layer 2.
