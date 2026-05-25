# ECP — Egent Code Plexus (structural code intelligence)

**Usage**: symbol-level code graph for AI agents. Sub-30ms queries; answers "who/what/impact", not "where's this string".

## The one rule

**Code structure → ecp. Text → grep.**

| You want to…                                         | Use                                              | Not  |
|------------------------------------------------------|--------------------------------------------------|------|
| Find a definition (function / class / type)          | `ecp find <name>` / `ecp inspect --name <name>`  | grep |
| Who calls / depends on X (before refactor/rename)   | `ecp impact --target <name> --direction upstream` | grep |
| Blast radius of a diff                               | `ecp impact --baseline <ref>`                    | manual trace |
| Routes / API contracts / event topics                | `ecp routes` / `ecp contracts` / `ecp find-event-mirrors` | grep |
| Cross-repo / arbitrary graph query                   | `ecp cypher '<query>'`                           | —    |
| String literal / config key / fs layout / vendored code | grep / glob                                   | ecp  |

When a task means "understand how this code connects" — reach for ecp first. grep only sees text; ecp understands scope, types, heritage, dispatch.

## Before any refactor / rename / signature change

Run `ecp impact --target <symbol> --direction upstream` to see callers. HIGH/CRITICAL risk → confirm with user.
