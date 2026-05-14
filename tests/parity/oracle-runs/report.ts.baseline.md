# verify-resolver report (ts)

Oracle records: 8396
Gnx records: 8052

## Summary

| class | count |
|---|---|
| TP (correct) | 350 |
| FP_ghost (wrong target) | 307 |
| FP_overmatch (Global with alts) | 255 |
| FN_dangling (oracle resolved, gnx didn't) | 36 |
| tier_demoted (correct but fell back to Global) | 350 |
| oracle_only (oracle imports gnx never saw) | 7409 |
| gnx_only same-file (excluded from diff) | 235 |
| gnx_only other (no oracle counterpart) | 6389 |

## Per-tier breakdown (gnx side)

| tier | TP | FP_ghost | FP_overmatch | tier_demoted | FN_dangling |
|---|---|---|---|---|---|
| Global | 350 | 305 | 255 | 350 | 0 |
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
