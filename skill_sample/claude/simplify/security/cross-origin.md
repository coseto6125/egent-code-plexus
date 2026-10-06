# Cross-origin surface

No Layer-1 rung. Read it when the diff touches CORS headers, cookie attributes, an embeddable widget, or a message channel between frames.

## Map it first

```bash
grep -rn "Access-Control-Allow\|SameSite\|postMessage\|addEventListener(\"message\"" .
ecp routes --format toon | grep -i "embed\|widget\|public"
ecp impact --target set_session_cookie --direction up
```

## Checks

1. **`Access-Control-Allow-Origin: *` and credentials never combine.** The browser refuses the pair, and code that reaches for both usually wants an origin allowlist instead.
2. **A wildcard belongs only on a resource with no authority.** A public script, a font, an icon. On an API that reads a session it is an open door.
3. **An origin allowlist is matched exactly.** A prefix or substring match lets `evil-example.com.attacker.net` through.
4. **`Access-Control-Allow-Origin` reflected from the request header is an allowlist of everything** unless the reflection happens after a membership check.
5. **A state-changing request authenticated by a cookie needs CSRF defence.** `SameSite=Lax` covers the common cases; a form that must work cross-site needs a token.
6. **`SameSite=None` requires `Secure` and a stated reason.** It is the setting an embedded widget needs, and the setting that reopens CSRF.
7. **A `postMessage` receiver checks `event.origin` against a fixed value**, and a sender names a target origin rather than `*`.
8. **An embeddable widget states who may embed it** — `frame-ancestors` in CSP, or `X-Frame-Options` — and the console that holds an operator session states that nobody may.
9. **A preflight-exempt request is still a request.** A simple `POST` with a form content type reaches the server without a preflight, so CORS alone never protects a state change.

## Known-safe — do not report these

| Shape | Why it is fine |
|---|---|
| `Access-Control-Allow-Origin: *` on a public embed script or static asset | Intended; it carries no authority and reads no cookie |
| `SameSite=Lax` on a session cookie | The working default; it survives a top-level OAuth return |
| `SameSite=None; Secure` on a widget cookie, with a comment saying why | Deliberate, and the CSRF answer is elsewhere |
| No CSRF token on an API authenticated by a header-borne bearer token | The browser does not attach it automatically |
| A `postMessage` to a same-origin iframe with an explicit target origin | This is the fix |

## Failure scenario, worked

> An embed widget's message receiver accepted any origin. A page that framed the widget posted a message the widget treated as a host command, reading the conversation it displayed. Precondition: getting the target to open a page. Authority obtained: another site's read of the widget's session content.
