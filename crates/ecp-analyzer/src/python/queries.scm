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

;; Routes
(call
  function: (attribute attribute: (identifier) @route.method (#match? @route.method "^(get|post|put|delete|patch|all|options|head|route|add_route|add_url_rule|add_api_route|GET|POST|PUT|DELETE|PATCH|ROUTE)$"))
  arguments: (argument_list [(string) @route.path (MISSING) @route.path])
) @route.call

