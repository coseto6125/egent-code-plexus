; Imports
(import_header
  (identifier) @import.source
  (import_alias (simple_identifier) @import.alias)?) @import

; Classes
(class_declaration
  (modifiers)? @export
  (type_identifier) @name.class
  (delegation_specifiers)? @heritage) @class

; Objects
(object_declaration
  (modifiers)? @export
  (type_identifier) @name.class
  (delegation_specifiers)? @heritage) @class

; Functions
(function_declaration
  (modifiers)? @export
  (simple_identifier) @name.function
  (user_type)? @type) @function
