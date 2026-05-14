;; Classes
(class_declaration
  (modifiers)? @export
  name: (identifier) @name.class
  interfaces: (super_interfaces)? @heritage
  superclass: (superclass)? @heritage) @class

;; Interfaces
(interface_declaration
  (modifiers)? @export
  name: (identifier) @name.interface
  interfaces: (extends_interfaces)? @heritage) @interface

;; Methods
(method_declaration
  (modifiers)? @export
  type: _ @type
  name: (identifier) @name.method) @method

;; Constructors
(constructor_declaration
  (modifiers)? @export
  name: (identifier) @name.method) @method

;; Imports
(import_declaration
  [
    (scoped_identifier
      name: (identifier) @import.name) @import.source
    (identifier) @import.name @import.source
  ]) @import
