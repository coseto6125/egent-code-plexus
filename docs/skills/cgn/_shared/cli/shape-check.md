# cgn shape-check

Detect drift between HTTP consumer access patterns and Route response shapes.

## Usage
```bash
cgn shape-check --route <PATH> [--repo <PATH>]
```

## Best For
- Finding if a client is reading keys that the server no longer sends.
- Verifying contract integrity between frontend and backend.
