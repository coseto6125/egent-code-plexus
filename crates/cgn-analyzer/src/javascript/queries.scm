;; Functions
(function_declaration
  name: (identifier) @name.function) @function

(export_statement
  declaration: (function_declaration
    name: (identifier) @name.function) @function) @export

;; Arrow Functions assigned to variables
(variable_declarator
  name: (identifier) @name.function
  value: (arrow_function)) @function

(export_statement
  declaration: (variable_declaration
    (variable_declarator
      name: (identifier) @name.function
      value: (arrow_function)) @function)) @export

;; Classes
(class_declaration
  name: (identifier) @name.class
  (class_heritage (expression) @heritage)?) @class

(export_statement
  declaration: (class_declaration
    name: (identifier) @name.class
    (class_heritage (expression) @heritage)?) @class) @export

;; Variables — module-level only (var / let / const not assigned to an arrow function).
;; Anchored to direct children of `program`; function-body / block-scope locals
;; are intentionally dropped (they bloat symbol counts without LLM-disambiguation
;; value). The export_statement patterns below already imply module scope.
(program
  (lexical_declaration
    (variable_declarator
      name: (identifier) @variable.name
    )
  ) @variable)

(program
  (variable_declaration
    (variable_declarator
      name: (identifier) @variable.name
    )
  ) @variable)

(export_statement
  declaration: (lexical_declaration
    (variable_declarator
      name: (identifier) @variable.name
    ) @variable
  )
) @export.variable

(export_statement
  declaration: (variable_declaration
    (variable_declarator
      name: (identifier) @variable.name
    ) @variable
  )
) @export.variable

;; Object property functions — { key: function(){} } style
;; Captures named function values inside object literals (e.g. res.format({html: fn}),
;; Express resource controllers, route handlers).
(pair
  key: [(property_identifier) (string (string_fragment))] @name.function
  value: [(function_expression) (arrow_function)] @function)

;; Methods
(method_definition
  name: (property_identifier) @name.method) @method

;; Imports (Named)
(import_statement
  (import_clause
    (named_imports
      (import_specifier
        name: (identifier) @import.name
        alias: (identifier)? @import.alias)))
  source: (string (string_fragment) @import.source)) @import

;; Imports (Default)
(import_statement
  (import_clause
    (identifier) @import.name)
  source: (string (string_fragment) @import.source)) @import

;; Re-exports — `export { X as Y } from 'lib'` (alias preserved on RawImport).
(export_statement
  (export_clause
    (export_specifier
      name: (identifier) @import.name
      alias: (identifier)? @import.alias))
  source: (string (string_fragment) @import.source)) @import

;; Namespace re-export — `export * as ns from 'lib'`.
;; `imported_name` is the "*" sentinel; `alias` holds the namespace binding.
(export_statement
  (namespace_export
    (identifier) @import.alias)
  source: (string (string_fragment) @import.source)) @import.namespace

;; Routes — generic method-call shape (.get/.post/... and .use with path-shaped string).
;; `use` is included here because router.use('/path', ...) and app.use('/path', ...)
;; register mount-point routes captured by ref-gitnexus. The path-shape filter in the
;; parser (clean_route_path) strips non-route strings so Map.get("key") is suppressed.
(call_expression
  function: (member_expression property: (property_identifier) @route.method (#match? @route.method "^(get|post|put|delete|patch|all|options|head|use|GET|POST|PUT|DELETE|PATCH)$"))
  arguments: (arguments [(string (string_fragment) @route.path) (MISSING) @route.path])
) @route.call

;; ---- framework queries ----

;; Express: app.{get,post,put,delete,patch,all,options,head}(<path_str>, <handler>)
;; `use` is INTENTIONALLY excluded — it mounts middleware, not a route, and
;; emitting a framework_ref for `app.use('/api', apiRouter)` would falsely
;; surface `apiRouter` as an HTTP handler (fixed: PR #2 review issue #3).
;;
;; The second argument can be an identifier (`handleUsers`), a member access
;; (`userRoutes.list`), or an inline function expression / arrow function.
;; All four shapes are captured; the parser textualises the handler node
;; (`<anonymous>` is used for inline functions where no symbolic target
;; exists) — fixed: PR #2 review issue #2.
;;
;; Gated downstream by `import ... 'express'`.
(call_expression
  function: (member_expression
    object: (identifier)
    property: (property_identifier) @express.route.method
    (#match? @express.route.method "^(get|post|put|delete|patch|all|options|head)$"))
  arguments: (arguments
    [(string) @express.route.path (MISSING) @express.route.path]
    [
      (identifier)
      (member_expression)
      (arrow_function)
      (function_expression)
    ] @express.route.handler))

;; Hapi: server.route({ method: 'GET', path: '/u', handler: getUsers })
;; Captures the handler identifier from the option-object pair. Gated downstream
;; by `import ... '@hapi/hapi'` (or `'hapi'`).
(call_expression
  function: (member_expression
    object: (identifier)
    property: (property_identifier) @hapi.route.kw
    (#eq? @hapi.route.kw "route"))
  arguments: (arguments
    (object
      (pair
        key: (property_identifier) @hapi.route.handler.key
        value: (identifier) @hapi.route.handler)
      (#eq? @hapi.route.handler.key "handler"))))
