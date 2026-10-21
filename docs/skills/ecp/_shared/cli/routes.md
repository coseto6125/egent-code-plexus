# ecp routes

List HTTP routes and their handler chains.

## Usage
```bash
ecp routes [<PATH_PATTERN>] [--method <VERB>] [--repo <PATH>]
```

## Best For
- Auditing the API surface of a service.
- Finding the handler for a specific endpoint.
- Seeing the upstream callers of a route.
- Security review: which middleware guards a route. Laravel routes carry `middleware`: route-file middleware, outer group to inner route, then the controller's own (`$this->middleware`, `HasMiddleware`) for that action; `withoutMiddleware` removals are applied. Kernel globals and middleware-group members (`api` stays `api`) are not included. `Guard::class` shows as the class its own file resolves it to. A route without the field has none recorded.
- When the controller's middleware turns on code ecp cannot read statically (a branch, a value only the runtime knows, a parent class or trait outside the index), the route carries none of the controller's middleware, and a `laravel-controller-middleware-dynamic` BlindSpot says why, in the file of the code that decides (the controller, its parent or its trait): `ecp inspect` on a symbol of that file shows the line and reason.

With a path, each handler carries `handlerKind`: `named` (a bound function or method), `inline_anonymous` (a closure; no node of its own), or `unresolved_action` (a Laravel action such as `'C@m'` that binds no single method of the index; `action` holds it as written, and no callers are listed).

`ANY` is one route for every verb (Laravel `Route::any`); `--method X` keeps it when X is GET, HEAD, POST, PUT, PATCH, DELETE or OPTIONS.
