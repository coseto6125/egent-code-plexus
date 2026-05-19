;; Module definitions (treat as top-level class scope)
(module_definition
  module_identity: (module_identity) @class.name) @class

;; Function definitions (public fun, entry fun, public entry fun, friend fun — all use function_definition).
;; Visibility is resolved in parser.rs by walking the `modifier` named children of the matched node.
(function_definition
  name: (function_identifier) @function.name) @function

;; Struct definitions
(struct_definition
  name: (struct_identifier) @struct.name) @struct

;; Constants
(constant
  name: (constant_identifier) @const.name) @const

;; Use declarations — use_module form: use <addr>::<module>
(use_declaration
  (use_module
    (module_identity
      module: (_) @import.name) @import.source)) @import

;; Use declarations — use_module_member form: use <addr>::<module>::Member
(use_declaration
  (use_module_member
    (module_identity) @import.source
    use_member: (use_member
      member: (identifier) @import.name))) @import

;; Alias — use <addr>::<module> as Alias  (module alias)
(use_declaration
  (use_module
    alias: (module_identifier) @typedef.name)) @typedef

;; Alias — use <addr>::<module>::Item as Alias  (single-member alias, no braces)
(use_declaration
  (use_module_member
    use_member: (use_member
      alias: (identifier) @typedef.name))) @typedef

;; Alias — use <addr>::<module>::{Item as Alias, ...}  (brace-grouped aliases)
(use_declaration
  (use_module_members
    use_member: (use_member
      alias: (identifier) @typedef.name))) @typedef
