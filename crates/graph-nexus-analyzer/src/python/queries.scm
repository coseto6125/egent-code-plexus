;; Functions
(function_definition
  name: (identifier) @function.name
  return_type: (_) @type) @function

(function_definition
  name: (identifier) @function.name) @function

;; Classes
(class_definition
  name: (identifier) @class.name
  superclasses: (argument_list (identifier) @heritage)) @class

(class_definition
  name: (identifier) @class.name
  superclasses: (argument_list (attribute) @heritage)) @class

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

;; Routes
(call
  function: (attribute attribute: (identifier) @route.method (#match? @route.method "^(get|post|put|delete|patch|all|options|head|route|GET|POST|PUT|DELETE|PATCH|ROUTE)$"))
  arguments: (argument_list (string) @route.path)
) @route.call
