# Routes and authorization

Deepens rungs 1, 2, 3, 4 and 13. Read it when the diff adds a route, changes a guard, or touches how a handler picks its tenant.

## Map the surface first

```bash
ecp routes --format toon                       # every route, with handler and file
ecp routes /api/o/<org>/knowledge/web          # one route: handlers + the EntryPoint chain that reaches it
ecp impact --target require_auth --direction up --depth 2 | head -50   # which handlers wear the guard
```

The gap between the first and the third command is the anonymous surface. Enumerate it once, then read only the handlers inside it.

## Checks

1. **Every route names its guard.** A decorator, a middleware the blueprint installs, or an inline check on the first lines of the body. A route with none is anonymous. Confirm anonymity from the route table, not from the handler's name.
2. **Authentication is not authorization.** A session proves who is calling. It does not prove they may act on this tenant, this bot, this conversation. Both checks appear, or the second one is a finding.
3. **The tenant comes from the session, the resource is scoped inside the query.** A handler that reads `org` from the path, fetches the row, then compares `row.org_id` to the session has already fetched another tenant's row — the timing is wrong even when the comparison is right. The tenant belongs in the `WHERE`.
4. **A resource identifier from the caller is scoped, not trusted.** `bot_slug`, `conversation_id`, `lead_id` and their kind reach the query alongside the session's tenant, never alone.
5. **Privilege grants live off the login path.** Code that raises a role, widens a tenant scope, or upgrades a plan runs from an operator command or an admin route. A grant reachable from a login, signup, or OAuth callback lets anyone who can create an account trigger it.
6. **Refusals on an untrusted path are uniform.** Before the caller is known to be entitled, "no session", "no such tenant", "not a member" and "member but not owner" return the same status and the same body. Differing refusals turn the endpoint into an existence oracle for tenants and accounts.
7. **A convenience route ships deleted.** A route, flag or env branch that exists to make development easier is removed before release, not wrapped in a runtime guard. A guard is one edit, one inverted boolean, or one misread env value away from being a production route.
8. **A deleted guard is re-established somewhere.** For every check the diff removes, name where the new code enforces it. Unfound is the finding.

## Known-safe — do not report these

| Shape | Why it is fine |
|---|---|
| A route with no session check that serves a static asset, a health probe, or a public marketing page | Anonymity is the intent; confirm from the response, not the path |
| A framework-level 503 or 404 that fires before the guard, for a dependency the operator configures | It describes the platform, is identical for every caller, and reveals no tenant |
| A handler with no tenant check whose blueprint installs a tenancy middleware | Verify the middleware actually matches this route's host and prefix, then leave it |
| Two routes returning different statuses where one is already behind the guard | Uniformity binds untrusted paths only |
| A guard applied by a decorator you cannot see in the diff | `ecp inspect --name <handler>` shows the decorators; read before flagging |

## Failure scenario, worked

> `GET /api/oauth/line/start?org=<slug>` carried no session check. An anonymous caller opened it with any org slug, completed the consent screen with their own LINE channel, and the callback bound that channel to a tenant they had never joined. Precondition: none. Authority obtained: message delivery for another tenant's customers.

That is the shape a finding here takes: the caller who should not reach it, and what they get.
