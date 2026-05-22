---
name: ecp-command-reference
description: Detailed `ecp` command reference aligned with `ecp --help` and `ecp admin --help`.
---

# ecp Command Reference

`ecp [OPTIONS] <COMMAND>`. `--graph` defaults to `.ecp/graph.bin`.

## Top-Level Commands

| Goal | Command |
|---|---|
| ONE symbol → signature + body + 1-hop edges + callers + 1-hop impact | `ecp inspect --name X --repo .` |
| ONE symbol → blast radius | `ecp impact X --direction upstream --repo .` |
| PR blast radius — symbol view | `ecp impact --baseline origin/main --repo .` |
| Find symbol by exact name | `ecp find "name" --repo .` |
| Find symbol by ranked search | `ecp find "fragment" --mode bm25 --repo .` |
| Cypher query escape hatch | `ecp cypher "MATCH ... RETURN ..." --repo .` |
| AST-aware multi-file rename | `ecp rename --symbol old --new-name new --dry-run --repo .` |
| HTTP route → handler → upstream callers | `ecp routes <path?> --repo .` |
| Cross-repo API contracts inventory | `ecp contracts --repo @all` |
| Route / contract delta — edge view | `ecp diff --section all --baseline <ref> --repo .` |
| Route response-shape drift detection | `ecp shape-check --route <path>? --repo .` |
| External HTTP / DB / Redis / queue usage | `ecp tool-map` |
| Registry health / freshness / blind spots | `ecp summary` (was `ecp coverage`; alias kept one release) |
| Multi-session peer collaboration | `ecp peers` |
| LLM-workflow audit over changed files | `ecp review` |

## Admin Subcommands

Use `ecp admin --help` for the full subcommand list. The admin namespace is the interactive / operational surface.

| Goal | Command |
|---|---|
| Interactive host-integration management | `ecp admin` |
| Install git / Claude Code hook integration | `ecp admin install-hook` |
| Check hook install status | `ecp admin status` |
| Build or refresh the graph | `ecp admin index --repo .` |
| Delete a repo's index data + registry entry | `ecp admin drop` |
| Remove orphan index dirs | `ecp admin prune` |
| Interactive TOML config editor | `ecp admin config` |
| Manage repo group membership | `ecp admin group` |
| List / inspect L1 sessions | `ecp admin sessions` |
| Run MCP server or list exposed tools | `ecp admin mcp serve` / `ecp admin mcp tools` |
| Diff resolver dump against language oracle | `ecp admin verify-resolver` |

## Help Routing

- `ecp --help` -> top-level command map
- `ecp admin --help` -> admin subcommand map
- `ecp <command> --help` -> command-specific flags and options

## Notes

- Prefer `ecp --help` for the top-level map instead of paraphrasing command names from memory.
- Prefer `ecp admin --help` for admin routing instead of using `ecp admin` interactively when you only need the reference.
