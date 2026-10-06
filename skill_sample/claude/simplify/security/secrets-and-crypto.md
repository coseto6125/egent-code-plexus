# Secrets and cryptography

Deepens rung 11. Read it when the diff handles a key, signs or verifies something, generates a token, or adds a log line near either.

## Map it first

```bash
ecp find "sign verify token secret" --mode bm25
ecp impact --target <sign_fn> --direction up      # every purpose one key serves
grep -rn "_SECRET\|_KEY\|_TOKEN" --include=*.py . | grep -v test
```

## Checks

1. **Keys are derived per purpose.** One master secret, distinct derived keys for sessions, CSRF state, share links and webhooks. A single key across purposes lets a token minted for one be presented as another.
2. **Randomness for a security value comes from a CSPRNG.** `secrets`, `crypto.randomBytes`, `os.urandom`. `random` is for sampling and UI.
3. **Comparison of a secret is constant-time.**
4. **A secret has no default.** A config read that falls back to a literal, a dev value, or an empty string means the production failure is silent. Absent config fails startup.
5. **Passwords use a password hash.** Argon2, scrypt, bcrypt. A general-purpose digest, salted or not, is not one.
6. **A digest chosen for speed is labelled.** MD5 and SHA-1 are fine for a cache key, a fingerprint, or a checksum, and a comment saying which prevents the next reviewer from re-litigating it.
7. **Secrets stay out of URLs.** A query string reaches logs, referrers, and proxy caches.
8. **Secrets stay out of error bodies and exception messages.** An exception carrying the connection string reaches the client the moment someone widens an error handler.
9. **Rotation is possible.** Verification accepts the previous key during a window, or the design states that rotation means a rebuild.

## Known-safe — do not report these

| Shape | Why it is fine |
|---|---|
| `hashlib.md5(file_bytes)` for a cache key or a content fingerprint | Not a security use |
| `random.random()` for sampling, jitter, or a UI shuffle | Not a security use |
| A key read from an env var with no fallback, that raises when missing | This is the correct shape |
| A test fixture containing a fake key | Test files are out of scope |
| A token in an `Authorization` header rather than a query string | This is the fix, not the finding |
| A secret in a `.env.example` that is obviously a placeholder | Confirm it is not the real one, then leave it |

## Failure scenario, worked

> One secret signed both the session cookie and an unauthenticated share link. A share link's payload, re-encoded, verified as a session. Precondition: holding any share link. Authority obtained: a forged session for the account named in the payload.
