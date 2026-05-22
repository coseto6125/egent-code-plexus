# ecp review

LLM-workflow audit aggregator. Runs multiple checks in one shot.

## Usage
```bash
ecp review --baseline origin/main
```

## Checks Performed
- `impact`: Blast radius of changed symbols.
- `summary`: Check for blind spots in changed files.
- `tool-map`: Egress changes (new external calls).
- `shape-check`: Route response drift.
- `diff`: Resolver binding tier degradation.

## Best For
- PR pre-check.
- Getting a high-signal summary of structural changes.
