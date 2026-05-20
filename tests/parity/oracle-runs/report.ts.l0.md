# verify-resolver report (ts)

Oracle records: 8396 (bad lines: 0)
Ecp records: 8052 (bad lines: 0)

## Summary

| class | count |
|---|---|
| TP (correct) | 364 |
| FP_ghost (wrong target) | 293 |
| FP_overmatch (Global with alts) | 184 |
| FN_dangling (oracle resolved, ecp didn't) | 36 |
| tier_demoted (correct but fell back to Global) | 191 |
| oracle_only (oracle imports ecp never saw) | 7409 |
| ecp_only same-file (excluded from diff) | 235 |
| ecp_only other (no oracle counterpart) | 6389 |

## Per-tier breakdown (ecp side)

| tier | TP | FP_ghost | FP_overmatch | tier_demoted | FN_dangling |
|---|---|---|---|---|---|
| Global | 191 | 291 | 184 | 191 | 0 |
| ImportScoped | 173 | 0 | 0 | 0 | 0 |
| SameFile | 0 | 2 | 0 | 0 | 0 |
| Unresolved | 0 | 0 | 0 | 0 | 36 |
| oracle | 0 | 0 | 0 | 0 | 0 |

## Top-20 worst offenders

| src_file | name | class | detail |
|---|---|---|---|
| integration/auto-mock/src/bar.service.ts | Injectable | oracle_only_resolved |  |
| integration/auto-mock/src/bar.service.ts | FooService | oracle_only_resolved |  |
| integration/auto-mock/src/foo.service.ts | Injectable | oracle_only_resolved |  |
| integration/auto-mock/test/bar.service.spec.ts | Test | oracle_only_resolved |  |
| integration/auto-mock/test/bar.service.spec.ts | BarService | oracle_only_resolved |  |
| integration/auto-mock/test/bar.service.spec.ts | FooService | oracle_only_resolved |  |
| integration/cors/e2e/express.spec.ts | NestExpressApplication | oracle_only_resolved |  |
| integration/cors/e2e/express.spec.ts | Test | oracle_only_resolved |  |
| integration/cors/e2e/express.spec.ts | AppModule | oracle_only_resolved |  |
| integration/cors/e2e/fastify.spec.ts | FastifyAdapter | oracle_only_resolved |  |
| integration/cors/e2e/fastify.spec.ts | NestFastifyApplication | oracle_only_resolved |  |
| integration/cors/e2e/fastify.spec.ts | Test | oracle_only_resolved |  |
| integration/cors/e2e/fastify.spec.ts | AppModule | oracle_only_resolved |  |
| integration/cors/src/app.controller.ts | Controller | oracle_only_resolved |  |
| integration/cors/src/app.controller.ts | Get | oracle_only_resolved |  |
| integration/cors/src/app.module.ts | Module | oracle_only_resolved |  |
| integration/cors/src/app.module.ts | AppController | oracle_only_resolved |  |
| integration/discovery/e2e/discover-by-meta.spec.ts | Test | oracle_only_resolved |  |
| integration/discovery/e2e/discover-by-meta.spec.ts | TestingModule | oracle_only_resolved |  |
| integration/discovery/e2e/discover-by-meta.spec.ts | DiscoveryService | oracle_only_resolved |  |
