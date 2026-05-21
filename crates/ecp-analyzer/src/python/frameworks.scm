;; Framework-aware queries for Python (Tier 1: FastAPI subset).
;; All captures use prefix `fastapi.` to namespace them clearly.

;; FastAPI: Depends(<callable>) inside parameter defaults — captures the callable identifier.
;; Emitted as RawFrameworkRef from the enclosing function (resolved via span containment).
(call
  function: (identifier) @_depends_fn (#eq? @_depends_fn "Depends")
  arguments: (argument_list
    [(identifier) @fastapi.depends.target (MISSING) @fastapi.depends.target]))

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
        [(identifier) @django.url.handler (attribute attribute: (identifier) @django.url.handler) (MISSING) @django.url.handler]))))

;; Django signals — Pattern A: `@receiver(<signal>, ...)` decorator on def.
;; Capture signal name (first positional arg) and decorated function name.
(decorated_definition
  (decorator
    (call
      function: (identifier) @_r (#eq? @_r "receiver")
      arguments: (argument_list
        . (identifier) @django.signal.receiver_name)))
  definition: (function_definition
    name: (identifier) @django.signal.receiver_handler))

;; Django signals — Pattern B: `<signal>.connect(<handler_ident>, ...)` direct call.
;; Match only when handler arg is a bare identifier (excludes lambda/attribute/call),
;; keeping coverage near 90% with high precision.
(call
  function: (attribute
    object: (identifier) @django.signal.connect_name
    attribute: (identifier) @_c (#eq? @_c "connect"))
  arguments: (argument_list
    . (identifier) @django.signal.connect_handler))

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

;; ---- Transaction boundary decorators (T10 family) ----
;; Django: `@transaction.atomic` on a function or method (bare marker).
(decorated_definition
  (decorator
    (attribute
      object: (identifier) @_tx_obj (#eq? @_tx_obj "transaction")
      attribute: (identifier) @_tx_attr (#eq? @_tx_attr "atomic")))
  definition: (function_definition
    name: (identifier) @tx.atomic.handler))

;; Django: `@transaction.atomic(...)` on a function or method (call form).
(decorated_definition
  (decorator
    (call
      function: (attribute
        object: (identifier) @_tx_obj (#eq? @_tx_obj "transaction")
        attribute: (identifier) @_tx_attr (#eq? @_tx_attr "atomic"))))
  definition: (function_definition
    name: (identifier) @tx.atomic.handler))

;; Pony ORM: `@db_session` (bare marker decorator) on a function or method.
(decorated_definition
  (decorator
    (identifier) @_dec (#eq? @_dec "db_session"))
  definition: (function_definition
    name: (identifier) @tx.db_session.handler))

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

;; ---- Blind spots: truly unresolvable patterns ----
;; Spec: docs/superpowers/specs/2026-05-15-blind-spots.md §3
;; Unlike fan-out (candidates can be enumerated), these patterns cannot even
;; be listed — runtime data drives the target. Emit BlindSpot metadata so
;; the LLM is told "I cannot see what this calls" rather than silently miss.

;; eval(...)
((call function: (identifier) @blind.eval)
  (#eq? @blind.eval "eval"))

;; exec(...)
((call function: (identifier) @blind.exec)
  (#eq? @blind.exec "exec"))

;; compile(...)
((call function: (identifier) @blind.compile)
  (#eq? @blind.compile "compile"))

;; importlib.import_module(...)
(call
  function: (attribute
    object: (identifier) @_mod
    attribute: (identifier) @blind.dynamic_import)
  (#eq? @_mod "importlib")
  (#eq? @blind.dynamic_import "import_module"))

;; __import__(...)
((call function: (identifier) @blind.builtin_import)
  (#eq? @blind.builtin_import "__import__"))

;; getattr(<not-self>, name_var)() — cross-object reflection.
;; Second arg must be an identifier (variable), not a string literal. Outer
;; call invokes the getattr result. The first arg must NOT be `self` —
;; that case is handled by the Phase 2 reflection fan-out above.
(call
  function: (call
    function: (identifier) @_g
    arguments: (argument_list
      .
      (identifier) @_obj
      .
      (identifier)))
  arguments: (argument_list)
  (#eq? @_g "getattr")
  (#not-eq? @_obj "self")) @blind.cross_getattr

;; ── Pydantic SchemaField ──
;; Captures typed class attributes on `class X(BaseModel)` bodies.
;; Each typed assignment becomes one RawSchemaField via the T4-1 dispatcher.
;;
;; The `(#eq? @_super "BaseModel")` predicate prevents false positives on
;; plain classes with type annotations (they look identical syntactically).
;; Captures: owner class name, field identifier, field type annotation text.
(class_definition
  name: (identifier) @pydantic.owner
  superclasses: (argument_list (identifier) @_super)
  body: (block
    (expression_statement
      (assignment
        left: (identifier) @pydantic.field
        type: (type) @pydantic.type))))
(#eq? @_super "BaseModel")
