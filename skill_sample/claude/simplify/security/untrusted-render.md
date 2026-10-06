# Rendering untrusted content

No Layer-1 rung: it costs nothing until a repo renders content it did not author. Read it when the diff puts a value into markup, a link, or a redirect.

Modern frameworks escape by default, so this surface is mostly about the places that opt out — and about content that arrives from a model or a crawler, which reviewers habitually treat as trusted.

## Map it first

```bash
ecp pattern -p 'dangerouslySetInnerHTML={{__html: $X}}' --lang tsx
grep -rn "innerHTML\|v-html\|mark_safe\|| *safe\|autoescape off" .
ecp impact --target <markdown_render_fn> --direction up      # who feeds the renderer
```

## Checks

1. **Every opt-out from auto-escaping is justified at the call site.** `dangerouslySetInnerHTML`, `v-html`, `mark_safe`, `|safe`, `innerHTML`, `Markup()`. Name where the value came from and why it is trusted.
2. **Model output and crawled content are untrusted.** They are the two sources a reviewer waves through. A model can be talked into emitting markup; a crawled page contains whatever the crawled site chose.
3. **A markdown renderer needs an HTML policy.** Most allow raw HTML through by default. Either disable it or sanitize the output with a real sanitizer.
4. **Sanitizing is done by a library, not a regex.** Hand-rolled tag stripping loses to nesting, encoding, and malformed markup.
5. **A URL from content is scheme-checked before it becomes an `href` or `src`.** `javascript:`, `data:` and `vbscript:` execute. Allowlist `http`, `https`, `mailto`.
6. **A redirect target from a caller is validated against a fixed set or a same-origin rule.** An open redirect is a phishing primitive and a token-leak channel.
7. **Escaping matches the context.** HTML body, attribute, URL component and JavaScript string each need a different one; a single `escape()` is wrong in three of them.
8. **A `Content-Security-Policy` that omits `unsafe-inline` blunts the whole class**, which is a mitigation to note, never a reason to leave an injection.

## Known-safe — do not report these

| Shape | Why it is fine |
|---|---|
| `{value}` in React, `{{ value }}` in Django or Vue | Auto-escaped |
| `innerHTML = "<b>Loading…</b>"` with a constant | Nothing varies |
| `dangerouslySetInnerHTML` fed by a sanitizer's output | Verify the sanitizer's config, then leave it |
| A markdown renderer configured with raw HTML disabled | The control is present; say which option does it |
| A redirect to a path-only value that cannot become absolute | Confirm it cannot start with `//` |

## Failure scenario, worked

> A carousel rendered a product description straight from crawled markup. A crawled page carried an `onerror` attribute, and it executed inside the operator console where the session cookie lives. Precondition: getting a page into a tenant's crawl. Authority obtained: the operator's session.
