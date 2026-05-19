# Gemini cgn Skill — Code Graph Nexus Workflow

This skill provides the structural-knowledge layer for autonomous AI coding agents.

## Core Mandates
- **Symbols over Strings**: Prefer `cgn find/inspect` over `grep`.
- **Blast Radius before Refactor**: Always run `cgn impact` before modifying shared code.
- **Automatic Indexing**: Commands auto-detect changes; rarely need manual `admin index`.

## Command Matrix

| Goal | Command |
|---|---|
| ONE symbol → signature + body + edges + callers | `cgn inspect --name X` |
| ONE symbol → blast radius (affected callers + risk) | `cgn impact X --direction upstream` |
| PR blast radius (staged changes vs baseline) | `cgn impact --baseline origin/main` |
| Find symbol by exact name | `cgn find "name"` |
| Find symbol by fragment / ranked search | `cgn find "fragment" --mode bm25` |
| Arbitrary graph query / Cypher escape hatch | `cgn cypher "MATCH (m:Method) WHERE m.name='X' RETURN m"` |
| AST-aware multi-file rename (No find-replace!) | `cgn rename --symbol old --new-name new --dry-run` |
| HTTP route → handler → upstream callers | `cgn routes <path?>` |
| Cross-repo API contracts (routes / queue / RPC) | `cgn contracts --repo @all` |
| Detect drift between consumer access and Route shape | `cgn shape-check --route <path>?` |
| Enumerate calls to external clients (HTTP/DB/Redis) | `cgn tool-map` |
| LLM-workflow audit (impact + drift + egress) | `cgn review --baseline <ref>` |
| Registry health / frameworks / blind spots | `cgn coverage` |

## Tool Optimization for Gemini CLI
1. **Code symbol** → `run_shell_command(command="cgn inspect --name X")`.
2. **String literals / config keys / fs layout** → `grep_search` / `glob`.
3. **Targeted Reading** → Use `read_file` with `start_line` and `end_line` on files identified by `cgn`.

## Multi-repo & Group Workflow
- **Selector**: Use `--repo @all` for registry-wide queries.
- **Groups**: Use `cgn group <verb> <name>` for operations scoped to a defined set of repositories.
