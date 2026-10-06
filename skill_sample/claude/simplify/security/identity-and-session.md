# Identity, session and account linking

Deepens rungs 5 and 6. Read it when the diff touches login, OAuth, a session cookie, or the code that decides two identities are the same person.

## Map it first

```bash
ecp find "session cookie oauth login" --mode bm25
ecp impact --target issue_session --direction up      # every place that mints authority
ecp impact --target parse_session --direction up      # every place that consumes it
```

Minting sites are the interesting set. A session issued outside the login flow is the first thing to explain.

## Checks

1. **A provider claim is used only where the provider says it verified it.** An `email` without `email_verified` is a string the caller typed into someone else's identity provider. Unverified, it never links to an existing account, never merges records, never elevates.
2. **Linking by a shared attribute is a takeover primitive.** "Same email, same person" hands an account to whoever can make a provider emit that email. Link on the provider subject; treat the email as a display field.
3. **A signed blob carries the account it was issued to.** State, invitation, magic link, share token — each names its subject, and the redeeming handler re-checks that subject's rights at redemption. A valid signature proves the blob's origin, not the redeemer's standing.
4. **The blob's rights are re-checked, not remembered.** Between issue and redemption a role can be revoked, a membership removed, a tenant deleted. Read the current state.
5. **Replay has a bound.** A timestamp, a nonce the server records, or a single-use row. Without one, a captured blob works forever.
6. **The state check runs before the branch that reveals anything.** Ordering matters: a handler that answers "consent declined" before validating state tells an attacker their forged state pointed at a real tenant.
7. **A cookie carrying authority sets its flags.** `HttpOnly` always; `Secure` on any HTTPS deployment; `SameSite` chosen deliberately — `Lax` keeps a top-level OAuth return working, `None` needs `Secure` and a reason.
8. **Session material is derived per purpose.** One secret signing sessions, CSRF state and share links alike lets a token from one purpose be presented as another.

## Known-safe — do not report these

| Shape | Why it is fine |
|---|---|
| An email stored for display with no `email_verified` gate | Only linking, merging and elevating need the verified claim |
| A cookie with no `max_age`, expiring with the browser session | A deliberate lifetime, not a missing one — check the production setter matches before flagging a helper |
| `SameSite=Lax` on a session cookie | Lax is the working default for a top-level OAuth redirect back into the app |
| A provider that always verifies, so the code hardcodes `True` | Confirm from the provider's docs, then leave it; a comment saying which provider it is belongs there |
| `secrets.compare_digest` on a value that is not secret | Harmless, not a finding |

## Failure scenario, worked

> Login resolved an account by `lower(email)` regardless of whether the provider verified it. An attacker registered the founder's address at a provider that does not verify, signed in, and landed inside the founder's existing account. Precondition: knowing the address. Authority obtained: every org that account owns.
