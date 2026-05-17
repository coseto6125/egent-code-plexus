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

; Variables — top-level val/var (direct child of source_file).
; Broad capture; parser.rs post-filters to exclude class-body and function-local ones.
(property_declaration
  (variable_declaration
    (simple_identifier) @variable.name)
) @variable
