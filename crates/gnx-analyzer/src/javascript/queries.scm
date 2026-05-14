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
  (extends_clause
    value: (identifier) @heritage)?) @class

(export_statement
  declaration: (class_declaration
    name: (identifier) @name.class
    (extends_clause
      value: (identifier) @heritage)?) @class) @export

;; Methods
(method_definition
  name: (property_identifier) @name.method) @method

;; Imports (Named)
(import_statement
  import: (import_clause
    (named_imports
      (import_specifier
        name: (identifier) @import.name
        alias: (identifier)? @import.alias)))
  source: (string (string_fragment) @import.source)) @import

;; Imports (Default)
(import_statement
  import: (import_clause
    (identifier) @import.name)
  source: (string (string_fragment) @import.source)) @import
