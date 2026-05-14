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
        ]
      )*
    )?
  )
) @struct

;; Interfaces
(type_spec
  name: (type_identifier) @interface.name
  type: (interface_type)) @interface

;; Methods
(method_declaration
  receiver: (parameter_list
    (parameter_declaration
      type: [
        (type_identifier) @type
        (pointer_type (type_identifier) @type)
      ]
    )
  )
  name: (field_identifier) @method.name
) @method

;; Functions
(function_declaration
  name: (identifier) @function.name) @function

;; Imports
(import_spec
  name: (package_identifier) @import.alias
  path: (string_literal) @import.source) @import

(import_spec
  path: (string_literal) @import.source) @import
