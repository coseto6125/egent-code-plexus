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

;; ---- Django ----
;; Django: `urlpatterns = [path("/x", handler, ...), ...]`.
;; Match `path()` calls only inside an assignment whose LHS identifier is `urlpatterns`,
;; so unrelated `path()` calls elsewhere in the file are not captured.
;; The handler argument can be a bare identifier (`login_view`) or an attribute
;; (`views.user_list`) — capture the trailing identifier in both shapes.
(assignment
  left: (identifier) @_pats (#eq? @_pats "urlpatterns")
  right: (list
    (call
      function: (identifier) @_path_fn (#eq? @_path_fn "path")
      arguments: (argument_list
        .
        (string)
        .
        (identifier) @django.url.handler))))

(assignment
  left: (identifier) @_pats (#eq? @_pats "urlpatterns")
  right: (list
    (call
      function: (identifier) @_path_fn (#eq? @_path_fn "path")
      arguments: (argument_list
        .
        (string)
        .
        (attribute
          attribute: (identifier) @django.url.handler)))))

;; ---- Celery ----
;; Celery: `@shared_task` (bare marker decorator) on a function definition.
(decorated_definition
  (decorator
    (identifier) @_dec (#eq? @_dec "shared_task"))
  definition: (function_definition
    name: (identifier) @celery.task.handler))

;; Celery: `@<obj>.task` (marker attribute) on a function definition.
(decorated_definition
  (decorator
    (attribute
      attribute: (identifier) @_dec (#eq? @_dec "task")))
  definition: (function_definition
    name: (identifier) @celery.task.handler))

;; Celery: `@<obj>.task(...)` (call attribute) on a function definition.
(decorated_definition
  (decorator
    (call
      function: (attribute
        attribute: (identifier) @_dec (#eq? @_dec "task"))))
  definition: (function_definition
    name: (identifier) @celery.task.handler))

;; ---- Reflection fan-out (Phase 2) ----
;; `getattr(self, name_var)(...)` — dynamic dispatch on `self`. The second
;; positional argument must be an `(identifier)` (not a `(string)`), so static
;; lookups like `getattr(self, "fixed")()` are excluded. The outer call's span
;; is the fan-out site; the inner `getattr` call confirms the shape.
(call
  function: (call
    function: (identifier) @_g (#eq? @_g "getattr")
    arguments: (argument_list
      .
      (identifier) @_obj (#eq? @_obj "self")
      .
      (identifier) @reflection.getattr.name_var))) @reflection.getattr.site
