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

;; Constructors — distinct NodeKind::Constructor (split from Method in Round 4
;; of 14-lang parity). Ref gitnexus emits these separately; the prior
;; @method capture flattened them into Method.
(constructor_declaration
  (modifiers [
    "public"
    "protected"
  ])? @export
  name: (identifier) @constructor.name
) @constructor

;; Fields (instance / class variables) — emit one Property per declarator
;; so `int x, y;` produces two Property nodes. Modifiers (`public` /
;; `protected`) flip @export for visibility-aware downstream filters.
(field_declaration
  (modifiers [
    "public"
    "protected"
  ])? @export
  type: (_) @type
  declarator: (variable_declarator
    name: (identifier) @property.name)
) @property

;; Local variables — `int x = 0;` inside method/constructor bodies. Ref
;; gitnexus emits Variable for these (~18k on .sample_repo Java corpus).
;; Per-declarator capture mirrors Property handling.
(local_variable_declaration
  type: (_) @type
  declarator: (variable_declarator
    name: (identifier) @variable.name)
) @variable

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
