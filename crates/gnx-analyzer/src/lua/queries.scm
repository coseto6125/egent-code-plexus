;; Functions — top-level: `function foo()` and `local function foo()`
;; (local function foo() is aliased to function_declaration in this grammar)
(function_declaration
  name: (identifier) @function.name) @function

;; Table-method form: `function M.foo()`
(function_declaration
  name: (dot_index_expression
    field: (identifier) @function.name)) @function

;; Colon-method form: `function obj:method()`
(function_declaration
  name: (method_index_expression
    method: (identifier) @function.name)) @function

;; Variable assigned a function: `local foo = function() end`
;; or global: `foo = function() end`
(assignment_statement
  (variable_list
    name: (identifier) @function.name)
  (expression_list
    value: (function_definition))) @function

;; Table-as-class heuristic: `local T = {}` — PascalCase filter applied in parser.rs
(variable_declaration
  (assignment_statement
    (variable_list
      name: (identifier) @struct.name)
    (expression_list
      value: (table_constructor)))) @struct

;; Constants / variables: `local x = value` (non-table, non-function)
(variable_declaration
  (assignment_statement
    (variable_list
      name: (identifier) @const.name))) @const

;; Imports: `require("module_name")`
(function_call
  name: (variable) @_fn
  arguments: (arguments
    (string
      content: (string_content) @import.source))
  (#eq? @_fn "require")) @import
