;; Function definitions (with optional @external/@internal/@view/@pure/@payable decorators)
(function_definition
  (identifier) @function.name) @function

;; Decorators on function definitions
(function_definition
  (decorator
    (identifier) @decorator))

;; State variable declarations (mapped to Const)
(variable_definition
  (identifier) @const.name) @const

;; Named constants (mapped to Const)
(constant_definition
  (identifier) @const.name) @const

;; Import statements — capture the module identifier as source
(import_statement
  (identifier) @import.source) @import
