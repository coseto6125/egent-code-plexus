; resource "type" "name" { ... }  → class named "type.name"
(block
  (identifier) @_block_type
  (string_lit (template_literal) @_res_type)
  (string_lit (template_literal) @class.name)
  (#eq? @_block_type "resource")) @class

; data "type" "name" { ... }  → class named "type.name"
(block
  (identifier) @_block_type2
  (string_lit (template_literal) @_data_type)
  (string_lit (template_literal) @class.name)
  (#eq? @_block_type2 "data")) @class

; module "name" { ... }  → class, source attribute → import
(block
  (identifier) @_block_type3
  (string_lit (template_literal) @class.name)
  (#eq? @_block_type3 "module")) @class

; module source attribute: source = "./path"
(block
  (identifier) @_block_type4
  (string_lit)
  (#eq? @_block_type4 "module")
  (block_start)
  (body
    (attribute
      (identifier) @_attr_name
      (expression
        (literal_value
          (string_lit
            (template_literal) @import.source)))
      (#eq? @_attr_name "source")))) @import

; variable "name" { ... }  → const
(block
  (identifier) @_block_type5
  (string_lit (template_literal) @const.name)
  (#eq? @_block_type5 "variable")) @const

; output "name" { ... }  → const (exported: module public interface)
(block
  (identifier) @_block_type6
  (string_lit (template_literal) @output.name)
  (#eq? @_block_type6 "output")) @const

; locals { key = ... }  → each attribute is a typedef (named alias for an expression)
(block
  (identifier) @_block_type7
  (#eq? @_block_type7 "locals")
  (block_start)
  (body
    (attribute
      (identifier) @typedef.name))) @typedef
