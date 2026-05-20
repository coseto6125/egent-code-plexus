# Gemini ecp Skill — EgentCodePlexus Workflow

This skill provides the structural-knowledge layer for autonomous AI coding agents.

## Core Mandates
- **Symbols over Strings**: Prefer `ecp find/inspect` over `grep`.
- **Blast Radius before Refactor**: Always run `ecp impact` before modifying shared code.
- **Automatic Indexing**: Commands auto-detect changes; rarely need manual `admin index`.

## Command Matrix

| Goal | Command |
|---|---|
| ONE symbol → signature + body + edges + callers | `ecp inspect --name X` |
| ONE symbol → blast radius (affected callers + risk) | `ecp impact X --direction upstream` |
| PR blast radius (staged changes vs baseline) | `ecp impact --baseline origin/main` |
| Find symbol by exact name | `ecp find "name"` |
| Find symbol by fragment / ranked search | `ecp find "fragment" --mode bm25` |
| Arbitrary graph query / Cypher escape hatch | `ecp cypher "MATCH (m:Method) WHERE m.name='X' RETURN m"` |
| AST-aware multi-file rename (No find-replace!) | `ecp rename --symbol old --new-name new --dry-run` |
| HTTP route → handler → upstream callers | `ecp routes <path?>` |
| Cross-repo API contracts (routes / queue / RPC) | `ecp contracts --repo @all` |
| Detect drift between consumer access and Route shape | `ecp shape-check --route <path>?` |
| Enumerate calls to external clients (HTTP/DB/Redis) | `ecp tool-map` |
| LLM-workflow audit (impact + drift + egress) | `ecp review --baseline <ref>` |
| Registry health / frameworks / blind spots | `ecp coverage` |

## Tool Optimization for Gemini CLI
1. **Code symbol** → `run_shell_command(command="ecp inspect --name X")`.
2. **String literals / config keys / fs layout** → `grep_search` / `glob`.
3. **Targeted Reading** → Use `read_file` with `start_line` and `end_line` on files identified by `ecp`.

## Multi-repo & Group Workflow
- **Selector**: Use `--repo @all` for registry-wide queries.
- **Groups**: Use `ecp group <verb> <name>` for operations scoped to a defined set of repositories.
