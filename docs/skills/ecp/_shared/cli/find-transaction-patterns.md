# ecp find-transaction-patterns

Detect Saga compensating-transaction name-pairs (Saga half; Outbox deferred pending T5-33).

## Usage
```bash
ecp find-transaction-patterns [--class <CLASS>] [--format <FORMAT>] [--repo <PATH>]
```

## Pattern
Scans for method name-pairs following the Saga pattern:
- Operation: `<verb>_<noun>`
- Compensator: `compensate_<verb>_<noun>` | `undo_<verb>_<noun>` | `rollback_<verb>_<noun>`

Both must share the same owner class.

## Confidence formula
| Condition                                          | Score |
|---------------------------------------------------|-------|
| Exactly one matching compensator on same class | 0.6   |
| + Calls edge: compensator → operation         | +0.2  |
| **Cap**                                           | 0.85  |

## Tier labels
- **POSSIBLY_RELATED**: Confidence ≥0.75.
- **BLIND_SPOT**: Confidence <0.75.

## Output
Default format: `json`. Supports `toon`, `text`.

Each pair includes:
- **operation** / **compensator**: Fully-qualified names (`Class.method`).
- **file** / **line**: Location of the operation.
- **confidence**: Score on [0.0, 0.85].
- **tier**: Classification based on confidence.
- **evidence**: `compensator_calls_operation` boolean.
- **requires_verification**: Always `true`; findings never auto-enter the graph.

## Scope limitation
**Outbox half intentionally deferred** — depends on `EventTopicMirror` (T5-33 / schema-field work). Once T5-33 lands, Outbox pairs will be detected alongside Saga pairs.

## When to use
- **Saga workflow audit**: Before refactoring a compensating method, check the `Calls` evidence to confirm it invokes the operation.
- **Transaction pattern discovery**: Surface all Saga pairs in a class to validate design intent.
- **Reliability analysis**: Identify incomplete compensators (confidence <0.75) that may indicate missing rollback logic.

## When NOT to use
- One-off method rename → use `ecp rename`.
- Finding callers of a method → use `ecp impact`.
- Exploring unrelated methods → use `ecp inspect`.
