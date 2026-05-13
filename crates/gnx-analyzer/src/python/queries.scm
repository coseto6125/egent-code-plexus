;; Functions
(function_definition
  name: (identifier) @name.function) @function

;; Classes
(class_definition
  name: (identifier) @name.class) @class

;; Imports (from ... import ...)
(import_from_statement
  module_name: (dotted_name) @import.source
  name: (dotted_name) @import.name) @import

;; Imports (from ... import aliased)
(import_from_statement
  module_name: (dotted_name) @import.source
  name: (aliased_import
    name: (dotted_name) @import.name)) @import

;; Imports (import ...)
(import_statement
  name: (dotted_name) @import.name) @import

(import_statement
  name: (aliased_import
    name: (dotted_name) @import.name)) @import
