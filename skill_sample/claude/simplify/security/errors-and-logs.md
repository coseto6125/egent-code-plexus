# Errors and logs

No Layer-1 rung. Read it when the diff adds an error path, widens an exception handler, or writes a log line near a credential or a caller's data.

Two failure directions live here: telling the caller too much, and telling the log too much.

## Map it first

```bash
ecp pattern -p 'logger.$M($$$A)' --lang py --limit 40
grep -rn "str(exc)\|repr(e)\|traceback" --include=*.py . | grep -v test
ecp impact --target <error_response_helper> --direction up
```

## Checks

1. **An error body carries a code, not an internal detail.** No stack trace, no SQL text, no connection string, no file path, no library version to an unauthenticated caller. The detail goes to the log with a correlation id the caller may quote.
2. **Errors on an untrusted path do not distinguish.** "No such tenant" and "not a member" are the same response; see [`routes-and-authz.md`](routes-and-authz.md).
3. **Timing does not distinguish either**, on a path where the difference is worth measuring — a login that returns fast for an unknown account and slow for a known one is an enumeration oracle.
4. **A widened `except` still re-raises what it cannot handle.** A bare catch that logs and continues turns a failed authorization check into a permitted request.
5. **Failure is closed.** When the check itself errors — the identity service is down, the policy cannot be read — the answer is refusal.
6. **Secrets stay out of logs.** Tokens, cookies, API keys, signatures, full request bodies from an auth endpoint. Debug logging that dumps a raw body needs a redaction pass or a hard scope.
7. **Caller-supplied text is not concatenated into a log line** without escaping newlines: injected `\n` forges log entries and breaks the parser downstream.
8. **The security-relevant events are logged at all.** A privilege grant, a failed authorization, a credential change, a tenant switch. An incident you cannot reconstruct is an incident you cannot bound.
9. **Personal data in logs has a reason and a retention.** Message bodies, emails, phone numbers.

## Known-safe — do not report these

| Shape | Why it is fine |
|---|---|
| A stack trace in a log, not in a response | That is where it belongs |
| A detailed error to an authenticated operator on an admin route | Audience matters; confirm the route's guard |
| `except` on a specific exception that logs and continues | The narrow catch is the design |
| A correlation id in the response body | Intended; it carries no detail |
| A debug log behind a flag that is off in production | Verify the flag's production value, then leave it |

## Failure scenario, worked

> A webhook handler logged the raw body before verifying the signature, to debug a platform issue. The bodies contained customer message content and the platform's own tokens, and the log shipper forwarded them to a third-party service. Precondition: none. Authority obtained: reading customer conversations out of the log pipeline.
