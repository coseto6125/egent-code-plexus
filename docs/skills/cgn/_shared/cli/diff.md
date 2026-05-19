# cgn diff

Show structural deltas (bindings, routes, contracts) between a baseline and current state.

## Usage
```bash
cgn diff --baseline <REF> [--section <bindings|routes|contracts|all>]
```

## Options
- `--baseline`: Branch, Tag, SHA, or `HEAD~N`.
- `--section`: Filter to specific delta types.
- `--format`: `toon` (compact key:value), `json`, `text`.
