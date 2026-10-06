# Outbound fetch of a caller-supplied URL

Deepens rung 10. Read it when the diff makes the server fetch a URL, a host, or a path that a caller can influence.

## Map it first

```bash
ecp tool-map --category http               # every outbound call site, with file and line
ecp impact --target <fetch_helper> --direction up --depth 6   # does an entry point reach it?
ecp pattern -p 'aiohttp.ClientSession($$$A)' --callers-of Owner.method --lang py
```

`tool-map` gives the sink inventory in one query. The work is deciding, per site, where the URL came from.

## Checks

1. **Trace the URL to its origin before anything else.** A URL from config, an env var, or a hardcoded constant is server-controlled and safe. A URL from a request body, a query string, a database row another tenant wrote, or a model tool argument is attacker-controlled.
2. **Validation happens after DNS resolution, not on the string.** A hostname check passes for a name whose A record points at `127.0.0.1` or `169.254.169.254`. Resolve, then judge the address.
3. **Loopback, link-local, private and unique-local ranges are refused.** `127.0.0.0/8`, `::1`, `169.254.0.0/16`, `fe80::/10`, `10/8`, `172.16/12`, `192.168/16`, `fc00::/7`. Cloud metadata lives at `169.254.169.254` on AWS, GCP, Azure and OCI alike, and returns instance credentials.
4. **Every redirect is re-checked.** The first response chooses the second target. A client following redirects with the check applied only to the first URL has no check.
5. **The scheme is allowlisted.** `http` and `https` only. `file://`, `gopher://` and `ftp://` reach places HTTP does not.
6. **A blocked port list is not a substitute for a blocked address list**, but it helps: an internal admin port on a public host is still internal.
7. **The response does not flow back verbatim.** A fetch whose body reaches the caller, a corpus, or a model context is an exfiltration channel, which raises the severity of everything above.
8. **The fetch has a timeout and a size cap.** Without them the sink is also a resource-exhaustion primitive.

## Known-safe — do not report these

| Shape | Why it is fine |
|---|---|
| `session.get(SETTINGS.API_URL)` or any base URL from env or config | Server-controlled, whatever the caller sends as a path suffix — check the suffix separately for path traversal |
| A fetch whose host is fixed and only the path segment varies | Judge the path, not the host |
| An SDK calling its own vendor endpoint | The host is the library's, not the caller's |
| A rejected bare IP literal, where dotted hostnames still pass | Half a control. Report it as incomplete, and say which bypass survives, rather than as absent |
| A fetch of a URL a tenant configured through an operator-reviewed flow | Lower severity, still in scope: state the role needed |

## Failure scenario, worked

> A crawl seed accepted any dotted hostname and explicitly allowed `localhost`. An org owner submitted `http://localhost:6457/`, reaching the framework's inspector API on the loopback interface, and separately pointed a domain they controlled at `169.254.169.254` to read instance metadata. The fetched body landed in the knowledge base, where asking the bot returned it. Precondition: org-owner role. Authority obtained: worker control, and cloud instance credentials.
