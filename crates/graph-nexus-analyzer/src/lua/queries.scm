;; Functions — top-level: `function foo()` and `local function foo()`
;; (local function foo() is aliased to function_declaration in this grammar)
;; is_exported discrimination is done in parser.rs by checking whether the
;; raw source at the node's start byte begins with "local".
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

;; Table-assigned method: `M.foo = function() end`
;; Capture the field name (`foo`) — the table name is captured separately so
;; the parser can include it in the emitted node name.
(assignment_statement
  (variable_list
    name: (dot_index_expression
      table: (identifier) @function.table
      field: (identifier) @function.name))
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

;; Imports: `require("module_name")` (any context)
(function_call
  name: (variable) @_fn
  arguments: (arguments
    (string
      content: (string_content) @import.source))
  (#eq? @_fn "require")) @import

;; Imports with local alias: `local M = require("module_name")`
;; Captures the binding name plus the inner `function_call` span; the parser
;; uses the inner span to deduplicate against the bare-require pattern above.
(variable_declaration
  (assignment_statement
    (variable_list
      name: (identifier) @import.alias)
    (expression_list
      value: (function_call
        name: (variable) @_req_a
        arguments: (arguments
          (string
            content: (string_content) @import.alias.source))) @import.inner)
    (#eq? @_req_a "require"))) @import.aliased

;; Metatable inheritance: `setmetatable(obj, {__index = Parent})`
;; `meta.child` is the table getting a metatable; `meta.parent` is the class
;; it inherits from. The parser appends `Parent` to `obj`'s heritage list.
(function_call
  name: (variable) @_setm
  arguments: (arguments
    (identifier) @meta.child
    (table_constructor
      (field
        name: (identifier) @_idx_key
        value: (identifier) @meta.parent)))
  (#eq? @_setm "setmetatable")
  (#eq? @_idx_key "__index")) @meta
