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

;; Enums
(enum_declaration
  name: (identifier) @enum.name
) @enum

;; Enum constants — `enum Status { ACTIVE, INACTIVE, PENDING }`
(enum_declaration
  body: (enum_body
    (enum_constant
      name: (identifier) @enum_constant.name) @enum_constant_node))

;; Anonymous lambdas passed as call arguments — emit an `<anonymous>` Function
;; node (only when the body contains a call) so attach_to_enclosing can host
;; those calls instead of dropping them. Filter (A): callback registration.
;; lambda_expression is a subtype of expression, which argument_list accepts.
(argument_list (lambda_expression) @function.anonymous)

;; Annotation types (@interface)
(annotation_type_declaration
  name: (identifier) @annotation.name
) @annotation

;; Annotation element declarations (String value() default ""; inside @interface)
;; Emitted as Method — same role as interface abstract methods.
(annotation_type_element_declaration
  type: (_) @type
  name: (identifier) @method.name
) @method

;; Decorators — each pattern repeats the name capture so `name_node` is
;; populated in the same match, enabling the merge path in parser.rs.
(class_declaration
  (modifiers [
    (annotation) @decorator
    (marker_annotation) @decorator
  ])
  name: (identifier) @class.name
) @class

(interface_declaration
  (modifiers [
    (annotation) @decorator
    (marker_annotation) @decorator
  ])
  name: (identifier) @interface.name
) @interface

(method_declaration
  (modifiers [
    (annotation) @decorator
    (marker_annotation) @decorator
  ])
  name: (identifier) @method.name
) @method

(constructor_declaration
  (modifiers [
    (annotation) @decorator
    (marker_annotation) @decorator
  ])
  name: (identifier) @constructor.name
) @constructor
