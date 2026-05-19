;; Structs
(struct_item
  (visibility_modifier)? @export
  name: (type_identifier) @struct_item.name) @struct

;; Enums
(enum_item
  (visibility_modifier)? @export
  name: (type_identifier) @enum_item.name) @enum

;; Traits
(trait_item
  (visibility_modifier)? @export
  name: (type_identifier) @trait_item.name) @trait

;; Free-standing functions (top-level or nested in mod). Restricted to
;; `source_file` / `mod_item` parents so impl-internal function_items don't
;; double-fire (they're separately captured below as `@method`).
(source_file
  (function_item
    (visibility_modifier)? @export
    name: (identifier) @function_item.name
    return_type: (_)? @type) @function)
(mod_item
  body: (declaration_list
    (function_item
      (visibility_modifier)? @export
      name: (identifier) @function_item.name
      return_type: (_)? @type) @function))

;; Trait-impl methods: `impl Trait for Type { fn m(&self) { ... } }`.
;; return_type is optional — `fn m()` with no `-> T` was previously skipped.
(impl_item
  trait: [
    (type_identifier)
    (generic_type)
  ] @heritage
  body: (declaration_list
    (function_item
      (visibility_modifier)? @export
      name: (identifier) @function_item.name
      return_type: (_)? @type) @method))

;; Inherent-impl methods: `impl Type { fn m(&self) { ... } }`. Tree-sitter
;; has no negative-predicate so we list this as a separate pattern that
;; matches when `trait:` is absent — both fire for trait impls but the
;; parser-side dedup keeps the higher-priority Method kind.
(impl_item
  body: (declaration_list
    (function_item
      (visibility_modifier)? @export
      name: (identifier) @function_item.name
      return_type: (_)? @type) @method))

;; Trait body methods — both abstract declarations (`fn m(&self);`) and
;; default implementations (`fn m(&self) { ... }`). Two patterns because
;; tree-sitter-rust uses distinct node kinds for body-less signatures
;; (`function_signature_item`) vs concrete defs (`function_item`).
(trait_item
  body: (declaration_list
    (function_signature_item
      (visibility_modifier)? @export
      name: (identifier) @function_item.name
      return_type: (_)? @type) @method))
(trait_item
  body: (declaration_list
    (function_item
      (visibility_modifier)? @export
      name: (identifier) @function_item.name
      return_type: (_)? @type) @method))

;; Associated types inside impl blocks: `type Item = T::Item;`
(impl_item
  body: (declaration_list
    (associated_type
      name: (type_identifier) @type_alias_item.name) @type_alias))

;; Associated types inside trait definitions: `type Item;`
(trait_item
  body: (declaration_list
    (associated_type
      name: (type_identifier) @type_alias_item.name) @type_alias))

;; macro_rules! definitions
(macro_definition
  name: (identifier) @macro_item.name) @macro_def

;; Struct fields (named-field structs only; tuple structs have no field_identifier)
(struct_item
  (visibility_modifier)? @export
  body: (field_declaration_list
    (field_declaration
      (visibility_modifier)? @export
      name: (field_identifier) @property.name) @property))

;; Enum variant struct-form fields: `enum E { V { f1: T, f2: U } }`. Each named
;; field is a permanent type-level data member parallel to struct fields, and
;; pattern-match destructuring `V { f1, f2 } => ...` references them by name.
(enum_variant
  body: (field_declaration_list
    (field_declaration
      (visibility_modifier)? @export
      name: (field_identifier) @property.name) @property))

;; Modules (both inline `mod foo { }` and declaration `mod foo;`)
(mod_item
  (visibility_modifier)? @export
  name: (identifier) @module_item.name) @module

;; Type aliases: `type Foo = Bar;`
(type_item
  (visibility_modifier)? @export
  name: (type_identifier) @type_alias_item.name) @type_alias

;; Constants: `const X: T = ...;`
(const_item
  (visibility_modifier)? @export
  name: (identifier) @const_item.name) @const_decl

;; Statics: `static X: T = ...;` / `static mut X: T = ...;` — semantically
;; another compile-time constant from the LLM's viewpoint. NodeKind::Static
;; doesn't exist; map to Const (ref-gitnexus uses a `Static` label, so the
;; per-side parity diff records this as a Const↔Static label_diff).
(static_item
  (visibility_modifier)? @export
  name: (identifier) @const_item.name) @const_decl

;; Impl blocks: `impl T` / `impl Trait for T`  (inherent and trait impls).
;; For `impl Foo<'a>` / `impl<T> Foo<T>` descend into `generic_type` so we
;; capture just the bare type_identifier — ref-gitnexus stores the type
;; name without generic parameters, so including `<'a>` here produces
;; spurious "Impl ref_only" parity drift on every generic impl block.
(impl_item
  type: [
    (type_identifier) @impl_item.name
    (generic_type type: (type_identifier) @impl_item.name)
  ]) @impl_block

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
