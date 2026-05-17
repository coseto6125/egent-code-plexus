;; Declarations
(class_declaration
  (modifiers (visibility_modifier) @export)?
  name: [
    (type_identifier)
    (user_type (type_identifier))
  ] @class.name
  (inheritance_specifier inherits_from: (user_type (type_identifier) @heritage))?
) @class

;; Functions — `func f(...) -> Bool` captures return type via the trailing
;; `(user_type (type_identifier))` field. tree-sitter-swift exposes the
;; return type as a sibling `name:` field on `function_declaration`, so we
;; capture it positionally rather than via a `result:` field.
(function_declaration
  (modifiers (visibility_modifier) @export)?
  name: (simple_identifier) @function.name) @function

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
;; KNOWN GAP: only typed-with-user_type properties get captured. Untyped
;; (`var z = 0`) and non-user_type (`[Int]`, `Int?`, tuples, function
;; types) declarations are missed. Relaxing this query to `?` produced
;; ~3× over-emission via tree-sitter pattern alternation that simple
;; (root, name) dedupe could not collapse. Dedicated round to follow.
(property_declaration
  name: (pattern bound_identifier: (simple_identifier) @property.name)
  (type_annotation
    (user_type (type_identifier) @property.type))) @property

;; Imports — Swift `import Module`. tree-sitter-swift grammar defines
;; `identifier: sep1(simple_identifier, _dot)`, so the module name sits
;; one level inside the `identifier` node as `(simple_identifier)`. The
;; nested capture mirrors upstream gitnexus's query (see
;; `_source_code/gitnexus/src/core/ingestion/tree-sitter-queries.ts:1251`).
;; Double-tagged because Swift basic-form imports have no separately-
;; named symbol part.
(import_declaration
  (identifier (simple_identifier) @import.name @import.source)
) @import

;; Typealias declarations — `typealias MyInt = Int` or generic
;; `typealias R<T> = Swift.Result<T, Error>`. Captured at the top level so the
;; parser can read lhs name + full rhs text (including generics) from the
;; @typealias node's byte range.
(typealias_declaration) @typealias
