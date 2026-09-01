# ecp heuristics

Three heuristic detectors behind one verb. Every finding carries
`requires_verification: true` and a confidence score; none enters the graph.

```bash
ecp heuristics saga             [--class <NAME>] [--saga-only|--outbox-only] [--format <F>] [--repo <PATH>]
ecp heuristics schema-bindings  <FIELD> [--format <F>] [--repo <PATH>]
ecp heuristics event-mirrors    [--topic <GLOB>] [--min-confidence <F>] [--format <F>] [--repo <PATH>]
```

Confidence means something different per kind, so read the kind's own section
before comparing two numbers.

## saga — compensating transactions and Outbox

Saga pairs come from `CompensatedBy` edges emitted at index time: an operation
`<verb>_<noun>` against `compensate_` / `undo_` / `rollback_<verb>_<noun>` on the
same owner class. Confidence starts at 0.6, gains 0.2 when a `Calls` edge runs
compensator → operation, and caps at 0.85. `POSSIBLY_RELATED` at ≥0.75,
`BLIND_SPOT` below.

Outbox findings are a query-time name scan instead: `OutboxEvent` /
`event_outbox` / `message_outbox` class names reachable through
`Calls` → `Publishes` within BFS depth 5. They are not backed by a graph edge.
`--saga-only` and `--outbox-only` split the two.

## schema-bindings — one field's mirrors across classes

Takes `Class.field` (owner-scoped) or a bare `field` (every `SchemaField` of that
name, plus the unlinked candidates). Surfaces `MirrorsField` edges.
Confidence ≥0.85 → `LIKELY_RELATED`, below → `BLIND_SPOT`. Evidence breaks down
per check: name, type, owner_class, bidirectional. Default format `toon`.

## event-mirrors — publisher ↔ subscriber per topic

Lists `EventTopicMirror` edges as `(publisher_fn, subscriber_fn, topic,
confidence)`. Edges are emitted at a flat `confidence=0.85`, so
`--min-confidence` above that returns nothing. `--topic 'orders/*'` globs the
canonical topic name. `--lib` parses but is a no-op: `FrameworkId` is not
persisted in the archived graph. Default format `text`.

## When to use

- Before renaming a field on a transactional entity, to see its mirrors in other
  domain models (`schema-bindings`).
- Before refactoring a compensator, to confirm it actually calls its operation
  (`saga`, reading the `Calls` evidence).
- To pair up publishers and subscribers of an event topic the call graph cannot
  connect, because the hop is a broker (`event-mirrors`).

## When NOT to use

- A plain rename across one codebase → `ecp rename`.
- Callers or blast radius of a symbol → `ecp impact`.
- One symbol's full context → `ecp inspect`.
