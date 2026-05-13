;; Classes
(class_declaration
  name: (identifier) @name.class) @class

;; Interfaces
(interface_declaration
  name: (identifier) @name.interface) @interface

;; Methods
(method_declaration
  name: (identifier) @name.method) @method

;; Constructors
(constructor_declaration
  name: (identifier) @name.method) @method

;; Imports
(import_declaration
  [
    (scoped_identifier
      name: (identifier) @import.name) @import.source
    (identifier) @import.name @import.source
  ]) @import
