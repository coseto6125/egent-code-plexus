# Security Policy

## Reporting a vulnerability

Use [GitHub private vulnerability reporting](https://github.com/coseto6125/egent-code-plexus/security/advisories/new)
(Security → Report a vulnerability). Reports are acknowledged on a best-effort
basis — this is a single-maintainer project; there is no SLA. Please do not
open public issues for exploitable bugs.

## Supported versions

Only the latest released `0.x` minor receives fixes. There are no backports.

## What ecp touches on your machine

Threat-model transparency — everything the tool reads or writes, so you can
audit the claims against the source:

| Surface | Path / endpoint | When |
|---|---|---|
| Graph cache | `~/.ecp/<repo>/` (graphs, registry, locks) | every indexed query |
| Telemetry (local only) | `~/.ecp/telemetry/<repo>/*.jsonl` | per CLI/MCP call; opt out with `ECP_NO_TELEMETRY=1`. Never leaves the machine |
| Git hook | `.git/hooks/reference-transaction` in the target repo | only via explicit `ecp admin install-hook`; an existing hook is backed up or chained, never silently replaced |
| Claude Code hooks | `~/.claude/settings.json` entries | only via explicit `ecp admin install-hook --claude-code`; removed by `ecp uninstall` |
| Network | `api.github.com/repos/coseto6125/egent-code-plexus/releases/latest` via `curl` | update check in `ecp admin doctor` / throttled session-start probe; nothing else is fetched, no code is downloaded or executed |

ecp never executes code from the repositories it indexes — analysis is
tree-sitter parsing only.

## Supply chain

Release artifacts and CI are hardened as follows:

- **Signed artifacts**: every release asset has a Sigstore bundle
  (`<asset>.sigstore.json`, cosign keyless) — verify with
  `cosign verify-blob --bundle <asset>.sigstore.json <asset> --certificate-identity-regexp 'github.com/coseto6125/egent-code-plexus' --certificate-oidc-issuer https://token.actions.githubusercontent.com`
- **SLSA build provenance**: binaries additionally carry a GitHub attestation —
  verify with `gh attestation verify <artifact> --repo coseto6125/egent-code-plexus`
- **SBOM**: a CycloneDX SBOM is attached to each GitHub release (this repo ships
  `Cargo.lock`, so the full dependency graph is reproducible from source too)
- CI actions are pinned to commit SHAs; runners use egress auditing
  (step-security/harden-runner)
- Continuous checks: `cargo audit`, CodeQL, dependency-review, OpenSSF Scorecard

Not yet in place (roadmap, contributions welcome): reproducible-build
verification.
