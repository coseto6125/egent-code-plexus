---
name: ecp
description: Exploring code structure or tracing value dependencies — reach here before grep, and never guess `ecp` syntax from memory. Command by question: definition→`ecp find`, who-calls/blast-radius→`ecp impact`, value origins/consumers→`ecp flow`, full context→`ecp inspect`, routes/contracts→`ecp routes`/`ecp contracts`. Flags and admin subcommands: help routing inside.
---

# ecp — Entry Point

Single entry point for the Codex-facing `ecp` skill set. Identify the task category, then open the matching guide — guides and `--help` are ground truth, not memory.

## Core directives

1. **Help routing.** `ecp --help` = top-level command map; `ecp admin --help` = admin subcommand map; `ecp <command> --help` = per-command flags. `ecp admin` without a subcommand launches the interactive TUI — never run it just to see the reference.
2. **Smallest command that fits.** If one subcommand's help page answers the task, use it before reading any broader reference.

## Decision tree

| If you need... | Open |
|---|---|
| Command names, flags, output formats, or admin subcommands | [`guides/command-reference.md`](./guides/command-reference.md) |
| Change review workflow for changed files | [`../simplify/SKILL.md`](../simplify/SKILL.md) |

## Value changes

Before declaring a computed-value change compatible, run `ecp flow --file <path> --line <n> --column <n>`.
Select `--subject value`, `binding`, or `return` according to the actual change.
Use `--direction backward` for origins. The query reads current sources without an index.
Check each consumer and unresolved boundary. A call chain does not prove value dependency.
Run `ecp review --baseline <ref> --include flow` after edits to compare before/current consumers.
Refresh results when source hashes change. Do not treat unresolved or truncated results as proof of no impact.
Use `ecp flow --help` for overlays, budgets, and cache controls.

Reading every file under `skill_sample/codex/` for one task means you skipped this table.
