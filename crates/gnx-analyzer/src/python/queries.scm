;; Functions
(function_definition
  name: (identifier) @function.name
  return_type: (type) @type) @function

(function_definition
  name: (identifier) @function.name) @function

;; Classes
(class_definition
  name: (identifier) @class.name
  superclasses: (argument_list (identifier) @heritage)?) @class

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
