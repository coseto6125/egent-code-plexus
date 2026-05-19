---
name: cgn-command-reference
description: Detailed `cgn` command reference aligned with `cgn --help` and `cgn admin --help`.
---

# cgn Command Reference

`cgn [OPTIONS] <COMMAND>`. `--graph` defaults to `.cgn/graph.bin`.

## Top-Level Commands

| Goal | Command |
|---|---|
| ONE symbol → signature + body + 1-hop edges + callers + 1-hop impact | `cgn inspect --name X --repo .` |
| ONE symbol → blast radius | `cgn impact X --direction upstream --repo .` |
| PR blast radius — symbol view | `cgn impact --baseline origin/main --repo .` |
| Find symbol by exact name | `cgn find "name" --repo .` |
| Find symbol by ranked search | `cgn find "fragment" --mode bm25 --repo .` |
| Cypher query escape hatch | `cgn cypher "MATCH ... RETURN ..." --repo .` |
| AST-aware multi-file rename | `cgn rename --symbol old --new-name new --dry-run --repo .` |
| HTTP route → handler → upstream callers | `cgn routes <path?> --repo .` |
| Cross-repo API contracts inventory | `cgn contracts --repo @all` |
| Route / contract delta — edge view | `cgn diff --section all --baseline <ref> --repo .` |
| Route response-shape drift detection | `cgn shape-check --route <path>? --repo .` |
| External HTTP / DB / Redis / queue usage | `cgn tool-map` |
| Registry health / freshness / blind spots | `cgn coverage` |
| Multi-session peer collaboration | `cgn peers` |
| LLM-workflow audit over changed files | `cgn review` |

## Admin Subcommands

Use `cgn admin --help` for the full subcommand list. The admin namespace is the interactive / operational surface.

| Goal | Command |
|---|---|
| Interactive host-integration management | `cgn admin` |
| Install git / Claude Code hook integration | `cgn admin install-hook` |
| Check hook install status | `cgn admin status` |
| Build or refresh the graph | `cgn admin index --repo .` |
| Delete a repo's index data + registry entry | `cgn admin drop` |
| Remove orphan index dirs | `cgn admin prune` |
| Interactive TOML config editor | `cgn admin config` |
| Manage repo group membership | `cgn admin group` |
| List / inspect L1 sessions | `cgn admin sessions` |
| Run MCP server or list exposed tools | `cgn admin mcp serve` / `cgn admin mcp tools` |
| Diff resolver dump against language oracle | `cgn admin verify-resolver` |

## Help Routing

- `cgn --help` -> top-level command map
- `cgn admin --help` -> admin subcommand map
- `cgn <command> --help` -> command-specific flags and options

## Notes

- Prefer `cgn --help` for the top-level map instead of paraphrasing command names from memory.
- Prefer `cgn admin --help` for admin routing instead of using `cgn admin` interactively when you only need the reference.
