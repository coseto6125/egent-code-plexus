;; Nim callable declarations: proc, func, method, iterator, template, macro
;; All 6 forms are mapped to @function.name via alternation.
([
  (proc_declaration     name: _ @function.name)
  (func_declaration     name: _ @function.name)
  (method_declaration   name: _ @function.name)
  (iterator_declaration name: _ @function.name)
  (template_declaration name: _ @function.name)
  (macro_declaration    name: _ @function.name)
] @function)

;; Type sections containing an object declaration (class-like)
(type_section
  (type_declaration
    (type_symbol_declaration name: _ @class.name)
    (object_declaration) @class))

;; Import statements: `import foo`, `import foo, bar`
(import_statement) @import

;; From-import: `from foo import bar`
(import_from_statement
  module: _ @import.source) @import

;; Top-level constants and lets
(const_section
  (variable_declaration
    (symbol_declaration_list
      (symbol_declaration name: _ @const.name)))) @const

(let_section
  (variable_declaration
    (symbol_declaration_list
      (symbol_declaration name: _ @const.name)))) @const
