;; Functions — definitions (with body)
(function_definition
  type: (_) @type
  declarator: [
    (function_declarator
      declarator: (identifier) @function.name)
    (pointer_declarator
      declarator: (function_declarator
        declarator: (identifier) @function.name))
  ]) @function

;; Functions — declarations (prototypes / forward-decls, no body)
;; Anchored to translation_unit to avoid capturing local `extern` re-declarations.
(translation_unit
  (declaration
    declarator: [
      (function_declarator
        declarator: (identifier) @function.name)
      (pointer_declarator
        declarator: (function_declarator
          declarator: (identifier) @function.name))
    ]) @function)

;; Structs
(struct_specifier
  name: (type_identifier) @struct.name) @struct

;; Unions (no separate NodeKind — emitted as Struct)
(union_specifier
  name: (type_identifier) @union.name) @union

;; Enums
(enum_specifier
  name: (type_identifier) @enum.name) @enum

;; Typedefs
(type_definition
  declarator: (type_identifier) @typedef.name) @typedef

;; Macros — object-like `#define X val`
(preproc_def
  name: (identifier) @macro.name) @macro

;; Macros — function-like `#define M(a) ...`
(preproc_function_def
  name: (identifier) @macro.name) @macro

;; Includes
(preproc_include
  path: [
    (string_literal) @import.source
    (system_lib_string) @import.source
  ]) @import

;; Function parameters — `int x` / `const char* s`.
;; Capture the outer `parameter_declaration` + the param's identifier; the
;; parser computes the type text as the source slice from the declaration
;; start to the identifier start, preserving original spacing and any
;; pointer / qualifier prefix (`const char*`, `int**`).
(parameter_declaration
  declarator: [
    (identifier) @param.name
    (pointer_declarator (identifier) @param.name)
    (pointer_declarator (pointer_declarator (identifier) @param.name))
    (array_declarator declarator: (identifier) @param.name)
  ]) @param

;; Struct / union field declarations — `int x;` / `char* name;`.
;; Same approach as params: source-slice the text before the field name.
(field_declaration
  declarator: [
    (field_identifier) @field.name
    (pointer_declarator (field_identifier) @field.name)
    (pointer_declarator (pointer_declarator (field_identifier) @field.name))
    (array_declarator declarator: (field_identifier) @field.name)
  ]) @field

;; Top-level variable declarations — `static const int N = 5;` / `int *p;`.
;; Captures the outer declaration; the parser slices [decl.start, name.start)
;; so `static const int` (with qualifiers) is included in the annotation.
(translation_unit
  (declaration
    declarator: [
      (init_declarator declarator: (identifier) @var.name)
      (init_declarator declarator: (pointer_declarator (identifier) @var.name))
      (identifier) @var.name
      (pointer_declarator (identifier) @var.name)
    ]) @var)
