;; Declarations
(class_declaration
  (attribute)* @decorator
  (modifiers (visibility_modifier) @export)?
  name: [
    (type_identifier)
    (user_type (type_identifier))
  ] @name.class
  (inheritance_specifier inherits_from: (user_type (type_identifier) @heritage))?
) @class

;; Functions
(function_declaration
  (attribute)* @decorator
  (modifiers (visibility_modifier) @export)?
  name: (simple_identifier) @name.function
  result: (type_identifier) @type ?) @function

;; Imports
(import_declaration
  (identifier) @import.source
) @import
