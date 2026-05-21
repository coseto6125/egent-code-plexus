# ecp find-schema-bindings

Surface MirrorsField heuristic edges and blind-spot candidates for a schema field.

## Usage
```bash
ecp find-schema-bindings <field> [--format <FORMAT>] [--repo <PATH>]
```

## Field format
- **Class.field** (owner-scoped): Match exact field on specific class only.
- **field** (bare): Match field across all classes; additionally surfaces unlinked candidates.

## Output
Default format: `toon` (key:value pairs). Supports `json`, `text`.

Each result includes:
- **confidence**: Score (0.0–1.0); source of tier label.
- **tier**: `LIKELY_RELATED` (≥0.85) or `BLIND_SPOT` (<0.85).
- **evidence**: Per-check breakdown (name, type, owner_class, bidirectional).
- **requires_verification**: Always `true`; findings never auto-enter the graph.

## When to use
- **Schema refactoring**: Before renaming a field on a transactional entity, check mirrors across domain models.
- **Data consistency checks**: Verify all mirrors of a key field are being updated together.
- **Cross-service contracts**: Identify peer fields in different service schemas that represent the same domain concept.

## When NOT to use
- Plain field rename across a single codebase → use `ecp rename`.
- Finding usages/callers → use `ecp impact` or `ecp find`.
- Exploring unrelated schema nodes → use `ecp inspect`.
