;; Function definitions. Decorators (@external/@view/@payable/etc.) are
;; collected via a parser-side child walk on the captured @function node —
;; a separate `(decorator (identifier) @decorator)` pattern would match in
;; an independent iteration and couldn't be merged back with the span.
(function_definition
  (identifier) @function.name) @function

;; State variable declarations (mapped to Const)
(variable_definition
  (identifier) @const.name) @const

;; Named constants (mapped to Const)
(constant_definition
  (identifier) @const.name) @const

;; Import statements — capture the module identifier as source
(import_statement
  (identifier) @import.source) @import
