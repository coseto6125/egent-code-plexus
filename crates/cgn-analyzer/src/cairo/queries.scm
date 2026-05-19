;; Free functions (top-level and inside impl/trait bodies)
(function_declaration
  signature: (function_signature
    name: (name) @function.name)) @function

;; Structs
(struct_declaration
  name: (name) @struct.name) @struct

;; Enums (treated as struct-like)
(enum_declaration
  name: (name) @struct.name) @struct

;; Modules (treated as class-like groupings)
(module_declaration
  name: (name) @class.name) @class

;; Impl blocks: base impl (no trait)
(impl_base
  name: (name) @class.name) @class

;; Impl blocks: trait impl (with heritage)
(impl_trait
  name: (name) @class.name
  trait: (_) @heritage) @class

;; Traits
(trait_declaration
  name: (name) @class.name) @class

;; Constants
(const_declaration
  name: (name) @const.name) @const

;; Imports
(import_declaration
  path: (path) @import.source) @import

;; Typedef: aliased use-import — `use path::to::Item as Alias;`
;; The `alias:` field is only present when `as Alias` appears; plain `use X;` is skipped.
(import_declaration
  alias: (name) @typedef.name) @typedef

;; Typedef: type alias — `type X = Y;`
(typealias_declaration
  name: (name) @typedef.name) @typedef
