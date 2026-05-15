;; Structs
(type_spec
  name: (type_identifier) @struct.name
  type: (struct_type
    (field_declaration_list
      (field_declaration
        !name
        type: [
          (type_identifier) @heritage
          (pointer_type (type_identifier) @heritage)
          (qualified_type) @heritage
        ]
      )*
    )?
  )
) @struct

;; Interfaces
(type_spec
  name: (type_identifier) @interface.name
  type: (interface_type
    [
      (method_elem name: (field_identifier))
      (type_identifier) @heritage
      (qualified_type) @heritage
    ]*
  )
) @interface

;; Methods
(method_declaration
  receiver: (parameter_list
    (parameter_declaration
      type: [
        (type_identifier) @type
        (pointer_type (type_identifier) @type)
        (qualified_type) @type
      ]
    )
  )
  name: (field_identifier) @method.name
  result: [
    (type_identifier) @type
    (pointer_type (type_identifier) @type)
    (qualified_type) @type
    (parameter_list) @type
  ]?
) @method

;; Functions
(function_declaration
  name: (identifier) @function.name
  result: [
    (type_identifier) @type
    (pointer_type (type_identifier) @type)
    (qualified_type) @type
    (parameter_list) @type
  ]?
) @function

;; Imports
(import_spec
  name: (package_identifier) @import.alias
  path: [ (interpreted_string_literal) (raw_string_literal) ] @import.source) @import

(import_spec
  path: [ (interpreted_string_literal) (raw_string_literal) ] @import.source) @import

;; Struct fields — per-name capture so `X, Y int` emits two Property nodes.
;; Matches at any depth, so fields of nested anonymous structs are also captured.
(field_declaration
  name: (field_identifier) @field.name
) @field

;; Routes
(call_expression
  function: (selector_expression
    field: (field_identifier) @route.method
      (#match? @route.method "^(GET|POST|PUT|DELETE|PATCH|OPTIONS|HEAD)$"))
  arguments: (argument_list
    (interpreted_string_literal) @route.path
  )
) @route.call
