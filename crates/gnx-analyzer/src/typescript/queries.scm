;; Functions
(function_declaration
  name: (identifier) @function.name
  return_type: (type_annotation)? @type) @function

(export_statement
  (function_declaration
    name: (identifier) @function.name
    return_type: (type_annotation)? @type) @function) @export

;; Arrow Functions assigned to variables
(variable_declarator
  name: (identifier) @function.name
  value: (arrow_function)) @function

(export_statement
  (variable_declaration
    (variable_declarator
      name: (identifier) @function.name
      value: (arrow_function)) @function)) @export

;; Classes
(class_declaration
  name: (identifier) @class.name
  (extends_clause value: [(identifier) (member_expression)] @heritage)?) @class

(export_statement
  (class_declaration
    name: (identifier) @class.name
    (extends_clause value: [(identifier) (member_expression)] @heritage)?) @class) @export

;; Methods
(method_definition
  name: (property_identifier) @method.name
  return_type: (type_annotation)? @type) @method

;; Interfaces
(interface_declaration
  name: (identifier) @interface.name
  (extends_clause [(identifier) (member_expression)] @heritage)?) @interface

(export_statement
  (interface_declaration
    name: (identifier) @interface.name
    (extends_clause [(identifier) (member_expression)] @heritage)?) @interface) @export

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
