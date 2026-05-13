;; Structs
(type_spec
  name: (type_identifier) @name.class
  type: (struct_type)) @class

;; Interfaces
(type_spec
  name: (type_identifier) @name.interface
  type: (interface_type)) @interface

;; Methods
(method_declaration
  name: (field_identifier) @name.method) @method

;; Functions
(function_declaration
  name: (identifier) @name.function) @function

;; Imports
(import_spec
  name: (package_identifier)? @import.name
  path: (string_literal) @import.source)
