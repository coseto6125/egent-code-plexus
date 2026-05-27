# Changelog

## v0.5.3 - 2026-05-27

### Features

- ecp usage — CLI usage dashboard + telemetry instrumentation (#481)
- surface L1 overlay symbols so uncommitted edits are findable (#480)

### Bug Fixes

- bound the registry flock so a dead holder can't freeze the machine (#475)
- cross-repo graph loads skip version check; PathLiteral precision; schema impact_traversal (#473)
- sweep orphan .tmp + prune ghost registry entries (#474)
- npm trusted publishing (OIDC) + README + idempotent re-runs (#472)

### Performance

- parse dirty files once, build only the needed providers (#479)
