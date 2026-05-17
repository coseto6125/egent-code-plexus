; Imports
(import_header
  (identifier) @import.source
  (import_alias (type_identifier) @alias)?) @import

; Classes
(class_declaration
  (modifiers
    (annotation)* @decorator
  )? @export
  (type_identifier) @class.name
  (delegation_specifier
      [
        (user_type (type_identifier) @heritage)
        (constructor_invocation (user_type (type_identifier) @heritage))
      ]
    )*
) @class

; Objects
(object_declaration
  (modifiers
    (annotation)* @decorator
  )? @export
  (type_identifier) @class.name
  (delegation_specifier
      [
        (user_type (type_identifier) @heritage)
        (constructor_invocation (user_type (type_identifier) @heritage))
      ]
    )*
) @class

; Primary constructors — class Foo(val x: Int) form.
; primary_constructor only appears in the tree when explicit params are present
; (class Foo without parens has no primary_constructor child), so this pattern
; does not over-emit for bare class declarations.
(class_declaration
  (type_identifier) @constructor.name
  (primary_constructor) @constructor)

; Secondary constructors — explicit constructor(...) blocks inside class_body.
; The name is implicit = enclosing class's type_identifier (same as primary).
(class_declaration
  (type_identifier) @constructor.name
  (class_body
    (secondary_constructor) @constructor))

; Functions
(function_declaration
  (modifiers
    (annotation)* @decorator
  )? @export
  (simple_identifier) @function.name
  (user_type)? @type) @function

; Properties — class-scoped only (val/var inside class_body).
; Top-level file-scoped `val`/`var` are excluded here (those belong to Variable round).
(class_body
  (property_declaration
    (variable_declaration
      (simple_identifier) @property.name)
  ) @property)

; Variables — top-level val/var (direct child of source_file only).
; Anchored to source_file so class-body property_declarations don't produce
; spurious duplicate Variable nodes alongside the @property capture above.
(source_file
  (property_declaration
    (variable_declaration
      (simple_identifier) @variable.name)
  ) @variable)
