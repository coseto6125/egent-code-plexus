# Cost and limits

Deepens rung 12. Read it when the diff adds an endpoint that spends money, does work the caller sizes, or fans out.

Availability is a security property, and in a product that pays per model call, so is the bill. The finding here is not "add rate limiting" — that is the generic ask the confidence anchors score 0. The finding is a specific path, a specific caller, and the specific unbounded quantity.

## Map it first

```bash
ecp routes --format toon                                  # which of these need no session?
ecp impact --target <expensive_fn> --direction up --depth 6
grep -rn "rate_limit\|throttle\|Semaphore\|max_pages\|max_tokens" --include=*.py .
```

## Checks

1. **An anonymous route that spends money names its ceiling.** Per IP, per tenant, per session — some key, some window, some number.
2. **A caller-sized quantity has a maximum.** Page counts, batch sizes, result limits, recursion depth, upload size. The maximum is enforced at the entry point, not deep inside where a second caller can bypass it.
3. **Rejecting beats clamping when the caller must know.** A silently clamped job reports success and delivers a fraction of what was asked; the operator finds out from the data.
4. **Retry has a bound and a backoff.** An unbounded retry loop against a failing dependency is a self-inflicted flood.
5. **A per-turn deadline exists where a model or a network call can hang.** A stream that connects and then goes silent defeats a read timeout, so the ceiling is wall-clock on the whole turn.
6. **Fan-out is bounded.** A semaphore, a queue, a worker cap. One request that starts N outbound calls is an amplifier.
7. **The expensive path is not reachable before authentication where it does not need to be.** Moving the spend behind a session is usually cheaper than metering it.

## Known-safe — do not report these

| Shape | Why it is fine |
|---|---|
| An expensive route behind a session, on a product where the tenant pays per use | Metering is a billing question, not a security one |
| No rate limit where an edge proxy or gateway enforces one | Verify the rule exists there before accepting it |
| An unbounded loop over a collection the server built | Server-controlled size |
| A retry with backoff and a cap | This is the fix |
| A generic "there is no rate limiting" observation | No specific path, no caller, no quantity: not a finding |

## Failure scenario, worked

> A public chat endpoint called a paid model per request with no per-caller ceiling. A single script drove the monthly budget in an afternoon, and the outage that followed hit every tenant. Precondition: knowing the public URL. Authority obtained: none — the impact is spend and availability, which is why severity comes from the bill, not the data.
