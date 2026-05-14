;; Structs
(struct_item
  (visibility_modifier)? @export
  name: (type_identifier) @name.class) @class

;; Enums
(enum_item
  (visibility_modifier)? @export
  name: (type_identifier) @name.class) @class

;; Traits
(trait_item
  (visibility_modifier)? @export
  name: (type_identifier) @name.interface) @interface

;; Functions
(function_item
  (visibility_modifier)? @export
  name: (identifier) @name.function
  return_type: (return_type type: (_) @type)?) @function

;; Methods in impl
(impl_item
  trait: [
    (type_identifier)
    (scoped_identifier)
  ] @heritage
  body: (declaration_list
    (function_item
      (visibility_modifier)? @export
      name: (identifier) @name.method
      return_type: (return_type type: (_) @type)?) @method))

;; Methods in trait
(trait_item
  body: (declaration_list
    (function_signature_item
      (visibility_modifier)? @export
      name: (identifier) @name.method
      return_type: (return_type type: (_) @type)?) @method))

;; Imports (use std::collections::HashMap)
(use_declaration
  argument: (scoped_identifier
    path: (_)? @import.source
    name: (identifier) @import.name)) @import

;; Imports (use something)
(use_declaration
  argument: (identifier) @import.name @import.source) @import

;; Imports (use std::collections::{HashMap, HashSet})
(use_declaration
  argument: (scoped_use_list
    path: (_) @import.source
    list: (use_list
      [
        (identifier) @import.name
        (use_as_clause
          path: (identifier) @import.name
          alias: (identifier) @import.alias)
      ]))) @import

;; Imports with direct alias (use std::io as stdio)
(use_declaration
  argument: (use_as_clause
    path: (_) @import.source @import.name
    alias: (identifier) @import.alias)) @import
