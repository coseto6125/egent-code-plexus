# Persona Inference

This file defines the rules the SKILL uses to derive a `persona` table from
already-loaded prompts + chat history. The agent **does not fish for additional
user files** — every rule's signal must be observable in the agent's existing
context window.

## Rule table

| Signal | Persona dimension | Default |
|---|---|---|
| CLAUDE.md contains "繁體中文" or "Traditional Chinese" | lang_pref = zh-TW | Wizard speaks 繁中 |
| CLAUDE.md contains "respond in" and "English" | lang_pref = en | Wizard speaks English |
| Chat contains "cargo" or "rust" or "Rust workspace" | install_pref = cargo-binstall | Recommend `cargo binstall egent-code-plexus` |
| Chat contains "brew" or "Homebrew" | install_pref = brew | Recommend `brew install` formula |
| Chat contains "monorepo" or "multi-repo" or "workspace" | scope_pref = group-heavy | Don't skip group phase |
| Chat contains "Cursor" or "cursor" | ide_pref = cursor | mcp phase writes Cursor config |
| Chat contains "Zed" | ide_pref = zed | mcp phase writes Zed config |
| Chat contains "VS Code" or "vscode" or "Continue" | ide_pref = vscode | mcp phase writes VS Code config |
| Chat shows existing Claude Code session | ide_pref = claude-code | mcp phase writes Claude Code config |
| (empty) | lang_pref = unknown | conservative |
| (empty) | install_pref = github-release-tarball | conservative |
| (empty) | scope_pref = single-repo | conservative |
| (empty) | ide_pref = unknown | conservative (ask user explicitly) |

## How the rules are applied

1. At the start of each phase, the agent re-runs the table top-down against
   its current context.
2. The first matching row for each persona dimension wins (specific signals
   beat the `(empty)` fallback).
3. If two specific rules conflict for the same dimension (e.g., chat mentions
   both `cargo` and `brew`), the agent asks the user to disambiguate.
4. The agent never persists this table — it only lives in the in-memory
   working state for this wizard session.

## Adding new rules

When adding a row, you must also add at least one matching fixture to
`tests/skill/persona-fixtures.yaml` and re-run
`tools/test-persona-rules.sh` to confirm consistency.
