# Guide: PR Impact Analysis (Blast Radius)

Use this guide before submitting a PR or when reviewing changes to assess the risk of a modification.

## 1. Pre-edit Check
Before changing a function:
- Run [`cgn impact <SYMBOL> --direction upstream`](../_shared/cli/impact.md).
- If risk is HIGH/CRITICAL, analyze if the change breaks shared contracts.

## 2. Post-edit Audit
After staging your changes:
- Run [`cgn review --baseline origin/main`](../_shared/cli/review.md).
- This aggregated check identifies impact, route drift, and egress changes.

## 3. Route Verification
If you touched a controller or route handler:
- Run [`cgn routes /path/to/route`](../_shared/cli/routes.md) to see the full chain.
- Run [`cgn shape-check`](../_shared/cli/shape-check.md) to detect client/server drift.
