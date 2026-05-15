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
  (class_heritage (identifier) @heritage)?) @class

(export_statement
  declaration: (class_declaration
    name: (identifier) @name.class
    (class_heritage (identifier) @heritage)?) @class) @export

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

;; Routes
(call_expression
  function: (member_expression property: (property_identifier) @route.method (#match? @route.method "^(get|post|put|delete|patch|all|options|head|GET|POST|PUT|DELETE|PATCH)$"))
  arguments: (arguments (string (string_fragment) @route.path))
) @route.call
