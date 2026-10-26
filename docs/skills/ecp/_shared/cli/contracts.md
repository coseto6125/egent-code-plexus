# ecp contracts

Pair HTTP routes with the fetch calls that reach them, in one repo or across repos. `ecp contracts` reads routes only: `--kind queue` and `--kind rpc` exit non-zero as not implemented, and a run that extracts nothing exits non-zero instead of printing an empty list. `ecp group contracts` also reads gRPC.

## Usage
```bash
ecp contracts --repo @all [--unmatched-only]
ecp group contracts <GROUP_NAME>
```

## Best For
- Verifying if a consumer is calling a provider correctly.
- Finding orphaned consumers after a provider change.
- Multi-repo drift detection.
