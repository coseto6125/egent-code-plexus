;; Contract declarations (mapped to Class)
(contract_declaration
  name: (identifier) @class.name) @class

;; Interface declarations (mapped to Class)
(interface_declaration
  name: (identifier) @class.name) @class

;; Library declarations (mapped to Class)
(library_declaration
  name: (identifier) @class.name) @class

;; Inheritance specifier: `contract Token is IToken`
(contract_declaration
  (inheritance_specifier
    ancestor: (user_defined_type (identifier) @heritage)))

(interface_declaration
  (inheritance_specifier
    ancestor: (user_defined_type (identifier) @heritage)))

;; Functions inside contracts/interfaces/libraries
(contract_body
  (function_definition
    name: (identifier) @method.name) @method)

;; Top-level free functions
(source_file
  (function_definition
    name: (identifier) @function.name) @function)

;; Modifier definitions (treated as methods)
(contract_body
  (modifier_definition
    name: (identifier) @method.name) @method)

;; Event definitions (treated as const-like entries for symbol resolution)
(contract_body
  (event_definition
    name: (identifier) @const.name) @const)

;; State variable declarations
(contract_body
  (state_variable_declaration
    (visibility)? @state_var.visibility
    name: (identifier) @state_var.name) @state_var)

;; Import directives: import "./IToken.sol"
(import_directive
  source: (string) @import.source) @import
