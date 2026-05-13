;; Functions
(function_declaration
  name: (identifier) @name.function) @function

;; Arrow Functions assigned to variables
(variable_declarator
  name: (identifier) @name.function
  value: (arrow_function)) @function

;; Classes
(class_declaration
  name: (identifier) @name.class) @class

;; Methods
(method_definition
  name: (property_identifier) @name.method) @method

;; Interfaces
(interface_declaration
  name: (identifier) @name.interface) @interface

;; Imports (Named)
(import_statement
  import: (import_clause
    (named_imports
      (import_specifier
        name: (identifier) @import.name)))
  source: (string (string_fragment) @import.source)) @import

;; Imports (Default)
(import_statement
  import: (import_clause
    (identifier) @import.name)
  source: (string (string_fragment) @import.source)) @import
