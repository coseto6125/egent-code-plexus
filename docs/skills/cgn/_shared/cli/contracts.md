# cgn contracts

Inspect cross-repo API contracts (HTTP, gRPC, Queue).

## Usage
```bash
cgn contracts --repo @all [--unmatched-only]
cgn group contracts <GROUP_NAME>
```

## Best For
- Verifying if a consumer is calling a provider correctly.
- Finding orphaned consumers after a provider change.
- Multi-repo drift detection.
