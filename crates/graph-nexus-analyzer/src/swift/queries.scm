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

;; Functions — `func f(...) -> Bool` captures return type via the trailing
;; `(user_type (type_identifier))` field. tree-sitter-swift exposes the
;; return type as a sibling `name:` field on `function_declaration`, so we
;; capture it positionally rather than via a `result:` field.
(function_declaration
  (attribute)* @decorator
  (modifiers (visibility_modifier) @export)?
  name: (simple_identifier) @name.function) @function

;; Function parameters — `name: Type`. tree-sitter-swift names BOTH children
;; `name:` (the simple_identifier is the parameter name, the user_type is the
;; type), so we match positionally inside the `(parameter ...)` node.
(parameter
  (simple_identifier) @param.name
  (user_type (type_identifier) @param.type)) @param

;; Property declarations — `var x: Int` / `let y: String`. Note
;; tree-sitter-swift accepts `name:` field only on `property_declaration`
;; (for the bound name), NOT on `type_annotation`, so we descend
;; positionally to read the type identifier.
(property_declaration
  name: (pattern bound_identifier: (simple_identifier) @property.name)
  (type_annotation
    (user_type (type_identifier) @property.type))) @property

;; Imports
(import_declaration
  (identifier) @import.source
) @import
