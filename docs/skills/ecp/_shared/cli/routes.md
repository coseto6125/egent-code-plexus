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

Each route row and each handler carries `handlerKind`:

| `handlerKind` | Meaning |
|---|---|
| `named` | The handler binds one declared function, method or class. |
| `inline_anonymous` | The handler is a closure or an inline function. A closure with a node prints that `<anonymous:L:C>` node. |
| `unresolved` | The route names a handler that binds no declaration of the index, such as a Laravel `'C@m'` action or a Rails `posts#index`. `action` holds the name as written. No callers are listed. A `route-unbound-handler` BlindSpot records it. |
| `absent` | The route has no handler, such as a gRPC method. A `route-handler-absent` BlindSpot records it. |

`ANY` is one route for every verb (Laravel `Route::any`); `--method X` keeps it when X is GET, HEAD, POST, PUT, PATCH, DELETE or OPTIONS.
