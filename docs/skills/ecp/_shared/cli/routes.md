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
- Security review: which middleware guards a route. Laravel routes carry `middleware`, outer group to inner route; a route without the field has none recorded.

`ANY` is one route for every verb (Laravel `Route::any`); `--method X` keeps it.
