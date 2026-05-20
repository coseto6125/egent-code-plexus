# ecp contracts

Inspect cross-repo API contracts (HTTP, gRPC, Queue).

## Usage
```bash
ecp contracts --repo @all [--unmatched-only]
ecp group contracts <GROUP_NAME>
```

## Best For
- Verifying if a consumer is calling a provider correctly.
- Finding orphaned consumers after a provider change.
- Multi-repo drift detection.
