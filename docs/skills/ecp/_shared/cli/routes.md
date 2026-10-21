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
- Security review: which middleware guards a route. Laravel routes carry `middleware`: route-file middleware, outer group to inner route, then the controller's own (`$this->middleware`, `HasMiddleware`) for that action; `withoutMiddleware` removals are applied. Kernel globals and middleware-group members (`api` stays `api`) are not included. A route without the field has none recorded.

`ANY` is one route for every verb (Laravel `Route::any`); `--method X` keeps it when X is GET, HEAD, POST, PUT, PATCH, DELETE or OPTIONS.
