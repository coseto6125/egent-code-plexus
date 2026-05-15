;; Functions
(function_declaration
  name: (identifier) @function.name
  return_type: (type_annotation (type_identifier) @type)?
) @function

(export_statement
  (function_declaration
    name: (identifier) @function.name
    return_type: (type_annotation (type_identifier) @type)?
  ) @function
) @export

;; Arrow Functions assigned to variables
(lexical_declaration
  (variable_declarator
    name: (identifier) @function.name
    value: (arrow_function)
  )
) @function

(export_statement
  declaration: (lexical_declaration
    (variable_declarator
      name: (identifier) @function.name
      value: (arrow_function)
    ) @function
  )
) @export

(variable_declaration
  (variable_declarator
    name: (identifier) @function.name
    value: (arrow_function)
  )
) @function

(export_statement
  declaration: (variable_declaration
    (variable_declarator
      name: (identifier) @function.name
      value: (arrow_function)
    ) @function
  )
) @export

;; Constants
(lexical_declaration
  (variable_declarator
    name: (identifier) @const.name
  )
) @const

(export_statement
  declaration: (lexical_declaration
    (variable_declarator
      name: (identifier) @const.name
    ) @const
  )
) @export

;; Variables
(variable_declaration
  (variable_declarator
    name: (identifier) @variable.name
  )
) @variable

(export_statement
  declaration: (variable_declaration
    (variable_declarator
      name: (identifier) @variable.name
    ) @variable
  )
) @export

;; Classes
(class_declaration
  (decorator)* @decorator
  name: (type_identifier) @class.name
  (extends_clause value: (identifier) @heritage)?
) @class

(export_statement
  (class_declaration
    (decorator)* @decorator
    name: (type_identifier) @class.name
    (extends_clause value: (identifier) @heritage)?
  ) @class
) @export

;; Methods
(method_definition
  name: (property_identifier) @method.name
  return_type: (type_annotation (type_identifier) @type)?
) @method

;; Interfaces
(interface_declaration
  name: (type_identifier) @interface.name
  (extends_clause value: (identifier) @heritage)?
) @interface

(export_statement
  (interface_declaration
    name: (type_identifier) @interface.name
    (extends_clause value: (identifier) @heritage)?
  ) @interface
) @export

;; Imports (Named)
(import_statement
  (import_clause
    (named_imports
      (import_specifier
        name: (identifier) @import.name
        alias: (identifier)? @import.alias
      )
    )
  )
  source: (string (string_fragment) @import.source)
) @import

;; Imports (Default)
(import_statement
  (import_clause
    (identifier) @import.name
  )
  source: (string (string_fragment) @import.source)
) @import

;; Re-exports — `export { X as Y } from 'lib'` (and `export { X } from 'lib'`).
;; Captured separately from regular imports so the alias is preserved on the
;; emitted RawImport (parser sets `imported_name = X`, `alias = Some(Y)`).
(export_statement
  (export_clause
    (export_specifier
      name: (identifier) @import.name
      alias: (identifier)? @import.alias))
  source: (string (string_fragment) @import.source)) @import

;; Namespace re-export — `export * as ns from 'lib'`. The local namespace
;; binding `ns` is captured as the alias; `imported_name` is "*" (sentinel
;; matching the namespace import convention).
(export_statement
  (namespace_export
    (identifier) @import.alias)
  source: (string (string_fragment) @import.source)) @import.namespace

;; Routes — `app.METHOD(path, handler)` form.
;; `route.handler` captures the named handler argument when present so the
;; builder can emit a `HandlesRoute` edge from the handler function back
;; to the Route node. Inline / anonymous handlers (arrow fn, fn literal)
;; are not captured and the edge is skipped — the Route node still lands.
(call_expression
  function: (member_expression property: (property_identifier) @route.method (#match? @route.method "^(get|post|put|delete|patch|all|options|head|GET|POST|PUT|DELETE|PATCH)$"))
  arguments: (arguments
    (string (string_fragment) @route.path)
    (identifier)? @route.handler)
) @route.call
