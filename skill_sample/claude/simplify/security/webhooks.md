# Inbound webhooks

Deepens rung 7. Read it when the diff adds a platform callback, changes signature handling, or registers a new channel adapter.

A webhook route is anonymous by construction: the platform holds no session. The signature *is* the authentication, so every other guard in the codebase is absent here.

## Map it first

```bash
ecp routes --method POST --format toon | grep -i "webhook\|callback"
ecp impact --target verify_signature --direction up      # which adapters actually call it
grep -rn "compare_digest" --include=*.py . | grep -v test
```

The interesting output is an adapter that appears in the first command and not the second.

## Checks

1. **The signature is verified over the raw body, before decoding.** Re-serializing a parsed body changes bytes, and the digest then covers something the platform never signed.
2. **Comparison is constant-time.** `hmac.compare_digest`, not `==`.
3. **Verification runs before any side effect** — before the database write, before the enqueue, before the log line that quotes the payload.
4. **A missing signature header is a rejection, not a skip.** An empty string must not compare equal to an empty computed digest.
5. **Each adapter verifies with its own platform's scheme.** Shared helpers drift: one platform signs the body, another signs a timestamp plus the body, another signs a query string. Read the platform's spec for a new adapter rather than reusing the neighbour's helper unread.
6. **A replayed delivery is dropped.** The platform's delivery id, recorded and checked. Platforms retry on timeout, so a handler without this double-charges, double-sends, or double-writes.
7. **The secret is per-channel, not per-deployment**, where the platform issues it that way. A shared secret makes one tenant's leak everyone's.
8. **The handler's response says nothing.** A body that differs for "unknown tenant" and "bad signature" tells a prober which channel ids are live.
9. **A new platform extends every place the old ones are enumerated** — the route, the dedup table, the conversation constraint, the id used for log correlation. Enumerations drift apart silently.

## Known-safe — do not report these

| Shape | Why it is fine |
|---|---|
| A verification helper called from a middleware rather than the handler | Confirm the middleware matches this route, then leave it |
| `==` on a value that is not a secret, such as a platform id | Constant-time comparison is for secrets |
| No dedup on a platform that guarantees at-most-once delivery | Rare; verify the claim in the platform's docs before accepting it |
| A test that constructs a valid signature | Test files are out of scope unless the review is about test security |

## Failure scenario, worked

> A new channel adapter parsed the payload, then verified the signature against the re-serialized body. Whitespace differences made every real delivery fail closed — and a payload crafted to survive re-serialization passed. Precondition: knowing the endpoint URL. Authority obtained: injecting messages into any tenant's conversation.
