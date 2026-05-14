;; Framework-aware queries for Python (Tier 1: FastAPI subset).
;; All captures use prefix `fastapi.` to namespace them clearly.

;; FastAPI: Depends(<callable>) inside parameter defaults — captures the callable identifier.
;; Emitted as RawFrameworkRef from the enclosing function (resolved via span containment).
(call
  function: (identifier) @_depends_fn (#eq? @_depends_fn "Depends")
  arguments: (argument_list
    (identifier) @fastapi.depends.target))

;; FastAPI: route decorators `@app.<method>("/path")` on function definitions.
;; Captures the decorator object (app/router), HTTP method, and decorated function name.
;; Emitted as RawFrameworkRef: app --fastapi-route-<method>--> handler.
(decorated_definition
  (decorator
    (call
      function: (attribute
        object: (identifier) @fastapi.route.app
        attribute: (identifier) @fastapi.route.method
        (#match? @fastapi.route.method "^(get|post|put|delete|patch)$"))))
  definition: (function_definition
    name: (identifier) @fastapi.route.handler))
