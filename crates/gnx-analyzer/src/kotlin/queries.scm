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
