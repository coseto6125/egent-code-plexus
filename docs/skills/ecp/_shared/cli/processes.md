# ecp processes

List or trace **Process** nodes — pre-detected execution-flow groups emitted at index time via Leiden community detection + BFS (`pass4_processes` in `builder.rs`). Use this when you want the actual function-call sequence inside a flow, not just blast radius.

## Usage

### List Process nodes

```bash
ecp processes [--community <ID>] [--limit N] [--repo <PATH>]
```

Each Process is labelled `"Entry → Terminal"` (e.g. `handle_request → emit_response`). Default limit 50.

### Trace one Process's full step sequence

```bash
ecp processes trace <PATTERN> [--limit N] [--repo <PATH>]
```

`<PATTERN>` substring-matches (case-insensitive) against Process labels. Output is the ordered Function / Method steps from Entry to Terminal — i.e. the actual call sequence inside the community. Default limit 5 matched Processes.

## Options
- `--format`: `toon` (default) / `json` / `text`.
- `--community <ID>`: list view only — restrict to a single community ID.
- `--limit <N>`: cap matched / listed Processes.

## When to use this over `ecp impact --direction down`

| Want | Use |
|---|---|
| "Who calls X and who do they call" (blast radius, depth-bounded) | `ecp impact <X> --direction down --depth N` |
| "Full ordered execution sequence inside this flow" (Entry → … → Terminal) | `ecp processes trace <pattern>` |

`processes trace` gives a single deterministic step list, no depth tuning. `impact --direction down` gives a caller-tree with risk levels but no execution order — branches are union'd, not sequenced.
