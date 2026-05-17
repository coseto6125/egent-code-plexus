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
;; `@field.type` captures the textual type once per field_declaration; all names
;; in `X, Y int` share that type.
(field_declaration
  name: (field_identifier) @field.name
  type: _ @field.type
) @field

;; Parameter declarations — emit a Variable node per param name with the
;; textual type. Covers `func`, `method`, and named returns (named-return
;; entries also use `parameter_declaration` inside `result: (parameter_list)`).
(parameter_declaration
  name: (identifier) @param.name
  type: _ @param.type
) @param

;; Top-level `var n int = ...` — only when explicit `type:` is present.
;; `n := 1` (short_var_declaration) has no type field and is intentionally
;; skipped so inferred-type vars get `type_annotation=None`.
(var_spec
  name: (identifier) @var.name
  type: _ @var.type
) @var

;; File-scope `var X T = ...` (single declaration, with or without explicit type).
;; Anchored to source_file so function-local `var` blocks are not captured here.
(source_file
  (var_declaration
    (var_spec
      name: (identifier) @variable.name))) @variable

;; File-scope `var ( X T; Y T )` (grouped declaration block).
(source_file
  (var_declaration
    (var_spec_list
      (var_spec
        name: (identifier) @variable.name)))) @variable

;; Short variable declarations — `x := expr` and `x, y := a, b`.
;; The `left:` side is an expression_list; we capture identifiers on the left.
;; Parent check in parser.rs ensures only direct children of expression_list
;; are kept (not identifiers inside selector_expression sub-nodes).
;; No type field exists in the grammar — type_annotation will be None.
(short_var_declaration
  left: (expression_list
    (identifier) @local.name)
) @local

;; Routes — `r.GET("/path", handler)`-style HTTP router method invocations.
;; Matches gin / echo / chi / fiber etc. (they all share this shape).
;; Ported from upstream gitnexus
;; `core/group/extractors/http-patterns/go.ts:23-39`. The handler capture
;; lets the parser emit a `RawFrameworkRef` gated by gin / echo imports.
(call_expression
  function: (selector_expression
    field: (field_identifier) @route.method
      (#match? @route.method "^(GET|POST|PUT|DELETE|PATCH|OPTIONS|HEAD)$"))
  arguments: (argument_list
    (interpreted_string_literal) @route.path
    (identifier)? @route.handler
  )
) @route.call
