# Guide: Multi-Repo & Contract Analysis

Use this guide when working in a microservices environment or a multi-repo group.

## 1. Sync the Group
Ensure your cross-links are up to date:
- Run [`ecp group sync <NAME>`](../_shared/cli/group.md).

## 2. Audit Contracts
- Run [`ecp group contracts <NAME> --unmatched`](../_shared/cli/group.md) to find orphaned consumers.
- Use [`ecp contracts --repo @all`](../_shared/cli/contracts.md) for a registry-wide view.

## 3. Cross-Repo Impact
If you change a provider's API:
- Run [`ecp group impact <NAME> --target <SYMBOL> --repo <PROVIDER>`](../_shared/cli/group.md).
- This shows which other repos call this symbol (via HTTP/gRPC/etc.).
