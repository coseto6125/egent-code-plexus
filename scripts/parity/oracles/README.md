# Resolver oracles

Per-language module-resolution oracles for the resolver verification harness.
Each oracle emits the JSONL contract defined in
`docs/specs/2026-05-15-resolver-oracle-harness.md` so that
`gnx verify-resolver` can diff our resolver's decisions against an
authoritative source.

## `ts_oracle.mjs`

Uses the TypeScript Compiler API directly (no `tsc` CLI fork) so it picks up
`tsconfig.json` `baseUrl` + `paths`, `package.json` `imports`, and the full
Node module-resolution algorithm. Walks the whole repo (skipping
`node_modules`, `dist`, `build`, `.git`, `.next`, `coverage`, `out`),
parses every `.ts/.tsx/.js/.jsx/.mjs/.cjs` file with `ts.createSourceFile`,
enumerates each binding from `import` / `export ... from` clauses, and
runs `ts.resolveModuleName` per specifier. One JSONL line per binding goes
to stdout; a 5-line summary (files / imports / bindings / resolved /
unresolved) goes to stderr.

Run:

```bash
node scripts/parity/oracles/ts_oracle.mjs <repoPath> > dumps/oracle.ts.jsonl
```

Requirements: Node >=18 and the `typescript` package must be resolvable
on the host (either inside the target corpus's `node_modules`, or in any
of the fallback hint directories listed at the top of the script — see
`RESOLVE_HINTS`). If `typescript` is not installed anywhere the script
locates, it exits with a clear message; install it via
`npm i -D typescript` in the corpus.

v1 limitations (documented in-script):

- Only the root `tsconfig.json` is consulted. Project references and
  per-package `tsconfig.json` in monorepos are not walked. The root
  `baseUrl` + `paths` covers the alias cases the harness cares about.
- Dynamic `import("x")`, bare `require("x")`, and CJS destructuring are
  intentionally skipped (no static binding name to track).
- `export * from "x"` is skipped (no named local binding).
