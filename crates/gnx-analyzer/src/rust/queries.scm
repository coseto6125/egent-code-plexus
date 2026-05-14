;; Structs
(struct_item
  (visibility_modifier)? @export
  name: (type_identifier) @struct_item.name) @class

;; Enums
(enum_item
  (visibility_modifier)? @export
  name: (type_identifier) @enum_item.name) @class

;; Traits
(trait_item
  (visibility_modifier)? @export
  name: (type_identifier) @trait_item.name) @interface

;; Functions
(function_item
  (visibility_modifier)? @export
  name: (identifier) @function_item.name
  return_type: (_)? @type) @function

;; Methods in impl
(impl_item
  trait: [
    (type_identifier)
    (generic_type)
  ] @heritage
  body: (declaration_list
    (function_item
      (visibility_modifier)? @export
      name: (identifier) @function_item.name
      return_type: (_) @type) @method))

;; Methods in trait
(trait_item
  body: (declaration_list
    (function_signature_item
      (visibility_modifier)? @export
      name: (identifier) @function_item.name
      return_type: (_) @type) @method))

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