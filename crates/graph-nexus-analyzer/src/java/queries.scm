;; Classes
(class_declaration
  (modifiers [
    "public"
    "protected"
  ])? @export
  name: (identifier) @class.name
  interfaces: (super_interfaces (type_list (_) @heritage))?
  superclass: (superclass (_) @heritage)?
) @class

;; Interfaces
(interface_declaration
  (modifiers [
    "public"
    "protected"
  ])? @export
  name: (identifier) @interface.name
  interfaces: (extends_interfaces (type_list (_) @heritage))?
) @interface

;; Methods
(method_declaration
  (modifiers [
    "public"
    "protected"
  ])? @export
  type: (_) @type
  name: (identifier) @method.name
) @method

;; Constructors
(constructor_declaration
  (modifiers [
    "public"
    "protected"
  ])? @export
  name: (identifier) @constructor.name
) @constructor

;; Imports — regular named import
(import_declaration
  [
    (scoped_identifier
      name: (identifier) @import.name) @import.source
    (identifier) @import.name @import.source
  ]
  .
  ";"
) @import

;; Imports — wildcard / on-demand import (com.foo.Bar.*)
(import_declaration
  (scoped_identifier) @import.source
  (asterisk) @import.wildcard
) @import

;; Decorators
(class_declaration
  (modifiers [
    (annotation) @decorator
    (marker_annotation) @decorator
  ])
) @class

(interface_declaration
  (modifiers [
    (annotation) @decorator
    (marker_annotation) @decorator
  ])
) @interface

(method_declaration
  (modifiers [
    (annotation) @decorator
    (marker_annotation) @decorator
  ])
) @method

(constructor_declaration
  (modifiers [
    (annotation) @decorator
    (marker_annotation) @decorator
  ])
) @constructor
