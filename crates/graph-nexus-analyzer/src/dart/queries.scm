;; Classes
(class_declaration
  name: (identifier) @class.name
  superclass: (superclass type: (type (_) @heritage))?
  (mixins (type (_) @heritage))?
  interfaces: (interfaces (type (_) @heritage))?) @class

;; Enums
(enum_declaration
  name: (identifier) @interface.name) @interface

;; Mixins — semantically closer to a trait (default-method container) than an
;; interface; map to NodeKind::Trait to match Kotlin mixin convention.
(mixin_declaration
  name: (identifier) @trait.name
  interfaces: (interfaces (type (_) @heritage))?) @trait

;; Constructors — method_declaration whose signature wraps a constructor_signature
;; (no return type, name matches class name). Named constructors have two
;; identifier children; we use the first (class name) as the span anchor but
;; keep the full "Foo.named" text via the last identifier child.
(method_declaration
  signature: (method_signature
    (constructor_signature
      name: (identifier) @constructor.name))) @constructor

;; Methods — capture full method_declaration so the span covers the body,
;; otherwise call-extraction can't attach call sites to the enclosing method.
(method_declaration
  signature: (method_signature
    (function_signature
      return_type: (type)? @type
      name: (identifier) @method.name))) @method

;; Typedefs — new-style: `typedef Callback = void Function(int);`
;; and old-style: `typedef int Compare(int a, int b);`.
;; In both cases the first (type_identifier) child of type_alias is the name.
(type_alias
  (type_identifier) @typedef.name) @typedef

;; Functions — capture full function_declaration (signature + body) so calls
;; inside the body land in this node's span. The bare function_signature
;; alternative is kept for top-level signatures without a body
;; (e.g. abstract / external declarations).
(function_declaration
  (function_signature
    return_type: (type)? @type
    name: (identifier) @function.name)) @function

(function_signature
  return_type: (type)? @type
  name: (identifier) @function.name) @function

;; Properties — `int x = 0;` / `final String y;`. The `(type ...)` sibling
;; (when present) carries the field's declared type; properties without an
;; explicit annotation (`var x = 0`, `dynamic v`) still match this capture
;; with `@type` unset.
(declaration
  (type (_) @type)?
  (initialized_identifier_list
    (initialized_identifier name: (identifier) @property.name))) @property

;; Getters inside a class — `Type get name => ...` / `Type get name { ... }`
;; method_signature wraps getter_signature when the getter is a class member.
(method_declaration
  signature: (method_signature
    (getter_signature
      return_type: (type)? @type
      name: (identifier) @property.name))) @property

;; Setters inside a class — `set name(Type v) { ... }`
(method_declaration
  signature: (method_signature
    (setter_signature
      return_type: (type)? @type
      name: (identifier) @property.name))) @property

;; Top-level getters — `Type get name => ...` outside any class.
(getter_declaration
  signature: (getter_signature
    return_type: (type)? @type
    name: (identifier) @property.name)) @property

;; Top-level setters — `set name(Type v) { ... }` outside any class.
(setter_declaration
  signature: (setter_signature
    return_type: (type)? @type
    name: (identifier) @property.name)) @property

;; Function / method parameters — `String name`, `int age`. tree-sitter-dart
;; exposes the type as an unlabeled `(type ...)` child of `formal_parameter`,
;; so we descend positionally.
(formal_parameter
  (type (_) @param.type)
  name: (identifier) @param.name) @param

;; Top-level variable declarations — `double pi = 3.14;` / `var x = 0;`.
;; Note: tree-sitter-dart mis-parses `typedef Foo = ...` as
;; top_level_variable_declaration; the Rust layer skips those (type == "typedef").
(top_level_variable_declaration
  (type (_) @var.type)?
  (initialized_identifier_list
    (initialized_identifier name: (identifier) @var.name))) @var

;; Top-level `final` / `const` declarations — `final String k = 'v';`.
;; tree-sitter-dart uses static_final_declaration_list for these.
(top_level_variable_declaration
  type: (type (_) @var.type)?
  (static_final_declaration_list
    (static_final_declaration name: (identifier) @var.name))) @var

;; Imports — Dart `import 'pkg.dart';`. tree-sitter-dart wraps imports
;; three levels deep: `import_or_export > library_import >
;; import_specification > configurable_uri`. Mirrors upstream gitnexus's
;; query (see `_source_code/.../tree-sitter-queries.ts`). Double-tagged
;; @import.name/@import.source because Dart basic-form imports have no
;; separately-named symbol part.
(import_or_export
  (library_import
    (import_specification
      (configurable_uri) @import.name @import.source))) @import
