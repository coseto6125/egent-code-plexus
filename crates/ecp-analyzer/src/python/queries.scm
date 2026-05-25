;; Functions
(function_definition
  name: (identifier) @function.name
  return_type: (_) @type) @function

(function_definition
  name: (identifier) @function.name) @function

;; Classes
(class_definition
  name: (identifier) @class.name
  superclasses: (argument_list (expression) @heritage)) @class

(class_definition
  name: (identifier) @class.name) @class

;; Export marker (Naming Convention: not starting with _)
((function_definition name: (identifier) @n) @export (#not-match? @n "^_"))
((class_definition name: (identifier) @n) @export (#not-match? @n "^_"))

;; Imports (from ... import ...)
(import_from_statement
  module_name: (dotted_name) @import.source
  name: (dotted_name) @import.name) @import

;; Imports (from ... import aliased)
(import_from_statement
  module_name: (dotted_name) @import.source
  name: (aliased_import
    name: (dotted_name) @import.name
    alias: (identifier) @import.alias)) @import

;; Imports (import ...)
(import_statement
  name: (dotted_name) @import.name) @import

(import_statement
  name: (aliased_import
    name: (dotted_name) @import.name
    alias: (identifier) @import.alias)) @import

;; Properties — class-body attributes (plain and type-annotated)
(class_definition
  body: (block
    (expression_statement
      (assignment
        left: (identifier) @property.name) @property)))

;; Properties — instance attributes assigned via self in any method
(class_definition
  body: (block
    (function_definition
      body: (block
        (expression_statement
          (assignment
            left: (attribute
              object: (identifier) @_self (#eq? @_self "self")
              attribute: (identifier) @property.name)) @property)))))

;; Variables — module-level assignments (plain `x = …` and annotated `x: T = …`).
;; Anchored to direct children of `module`; function-body and class-body locals
;; are intentionally dropped (they bloat symbol counts without LLM-disambiguation
;; value; locals lack stable cross-file identity).
;; Both forms produce an `assignment` node in tree-sitter-python; the annotated
;; form additionally has a `type:` field, but the `left:` field is present in
;; both, so a single pattern suffices.
(module
  (expression_statement
    (assignment
      left: (identifier) @variable.name) @variable))

;; Decorators — attach raw decorator text to the parent function or class so
;; downstream consumers (Task #10/11 flag wiring, Tier 3 route detectors) can
;; read them.  Two sub-patterns per definition type:
;;   • non-call: `@property`, `@staticmethod`, `@functools.cached_property`
;;     → capture the identifier or dotted-attribute as-is.
;;   • call:     `@app.get("/users")`, `@click.command()`
;;     → capture only the call *target* (`function:` field), dropping arguments.
;;     Requirement: `@app.get("/users")` → "app.get", not "app.get(\"/users\")".
;;
;; Each pattern also re-captures `@function` / `@class` (the inner definition
;; node) so the parser's span-dedup loop merges decorators onto the same
;; RawNode that the plain function/class patterns create.

;; function — non-call decorator (@property, @staticmethod, @functools.lru_cache …)
(decorated_definition
  (decorator [(identifier) (attribute)] @decorator)
  definition: (function_definition
    name: (identifier) @function.name) @function)

;; function — call decorator (@app.get("/path"), @click.command() …)
(decorated_definition
  (decorator (call function: (_) @decorator))
  definition: (function_definition
    name: (identifier) @function.name) @function)

;; class — non-call decorator
(decorated_definition
  (decorator [(identifier) (attribute)] @decorator)
  definition: (class_definition
    name: (identifier) @class.name) @class)

;; class — call decorator
(decorated_definition
  (decorator (call function: (_) @decorator))
  definition: (class_definition
    name: (identifier) @class.name) @class)

;; Anonymous lambdas in call-argument position. Without a node here, any call
;; inside the lambda body is dropped by attach_to_enclosing when no named
;; enclosing scope exists — filter (A) callback registration. parser.rs only
;; emits a node when the body contains a call, so empty lambdas add no bloat.
(argument_list
  (lambda) @function.anonymous)

(argument_list
  (keyword_argument
    value: (lambda) @function.anonymous))

;; Routes
(call
  function: (attribute attribute: (identifier) @route.method (#match? @route.method "^(get|post|put|delete|patch|all|options|head|route|add_route|add_url_rule|add_api_route|GET|POST|PUT|DELETE|PATCH|ROUTE)$"))
  arguments: (argument_list [(string) @route.path (MISSING) @route.path])
) @route.call
