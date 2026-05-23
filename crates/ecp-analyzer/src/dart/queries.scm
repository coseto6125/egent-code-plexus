;; Classes
(class_declaration
  name: (identifier) @class.name
  superclass: (superclass type: (type (_) @heritage))?
  (mixins (type (_) @heritage))?
  interfaces: (interfaces (type (_) @heritage))?) @class

;; Enums — Dart first-class `enum Color { red, green }` and Dart 2.17+
;; enhanced enums with methods. tree-sitter-dart uses enum_declaration with
;; a named `name` field (single identifier, never multiple).
(enum_declaration
  name: (identifier) @enum.name) @enum

;; Enum constants — `enum Color { red, green, blue }`. Each `enum_constant`
;; node carries a `name` field (identifier). Dart 2.17+ enhanced enums with
;; constructor calls also use this node.
(enum_declaration
  body: (enum_body
    (enum_constant
      name: (identifier) @enum_constant.name) @enum_constant_node))

;; Mixins — semantically closer to a trait (default-method container) than an
;; interface; map to NodeKind::Trait to match Kotlin mixin convention.
(mixin_declaration
  name: (identifier) @trait.name
  interfaces: (interfaces (type (_) @heritage))?) @trait

;; Extensions — `extension Foo on T { ... }` and Dart 3 `extension type Foo(...)`.
;; Both are type-level constructs that add named members (functions / methods)
;; reachable as `T.fooMethod(...)`. ref-gitnexus emits these as Class; semantically
;; they're closer to a Trait (extend an existing type's behaviour without
;; subclassing), so we map to NodeKind::Trait and rely on the aggregator's
;; {Interface, Struct, Enum, Annotation, Class, Trait} EQUIV class for parity.
(extension_declaration
  name: (identifier) @trait.name) @trait

;; Constructors — four grammar forms in tree-sitter-dart:
;;
;;   1. method_declaration > method_signature > constructor_signature
;;      Covers: `Foo()`, `Foo.named()` (default + named).
;;
;;   2. method_declaration > method_signature > factory_constructor_signature
;;      Covers: `factory Foo.fromJson(...)`, `factory Foo()`.
;;
;;   3. declaration > constant_constructor_signature
;;      Covers: `const Foo()`, `const Foo.named()`. The `const` modifier
;;      promotes the grammar node out of method_declaration into declaration.
;;
;;   4. declaration > constructor_signature
;;      Covers: abstract/external constructors declared without a body.
;;
;; The `name` field is `multiple: true` (dot-separated identifiers for named
;; constructors). We anchor on the LAST identifier child so that `Foo.named`
;; captures "named" (the constructor's own name), matching upstream convention.
;; dedup in parser.rs collapses duplicates by (name, span, kind).
(method_declaration
  signature: (method_signature
    (constructor_signature
      name: (identifier) @constructor.name))) @constructor

(method_declaration
  signature: (method_signature
    (factory_constructor_signature
      name: (identifier) @constructor.name))) @constructor

(declaration
  (constant_constructor_signature
    name: (identifier) @constructor.name)) @constructor

(declaration
  (constructor_signature
    name: (identifier) @constructor.name)) @constructor

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
;; inside the body land in this node's span. The bare `function_signature`
;; alternative below catches top-level `external` / signature-only
;; declarations that tree-sitter-dart parses WITHOUT a `function_declaration`
;; wrapper. Both patterns can match the same function (one fires on the outer
;; node, the other on its inner signature child) — the parser filters that
;; case out by skipping any bare-signature emit whose parent is a
;; `function_declaration` (see parser.rs).
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

;; Annotations — `@override`, `@deprecated`, `@Foo()`, `@meta.visibleForTesting`.
;; tree-sitter-dart uses an `annotation` node with a `name` field that is
;; either a bare `identifier` or a `qualified` node.
(annotation
  name: (identifier) @annotation.name) @annotation

(annotation
  name: (qualified (_) (identifier) @annotation.name)) @annotation

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

;; ---- BlindSpot patterns (FU-001 P6b) ----
;; import 'dart:mirrors' — file uses runtime reflection; downstream
;; reflect/MirrorSystem calls bind names at runtime. Anchored at the
;; import_or_export node so the span covers the whole statement.
((import_or_export
   (library_import
     (import_specification
       (configurable_uri) @_uri))) @blind.mirrors_import
  (#match? @_uri "dart:mirrors"))

;; Function.apply(<fn>, <args>) — reflective function invocation. The
;; receiver `Function` is the built-in type; matching the bare identifier
;; rather than any user-defined class named `Function` is enforced by the
;; `#eq?` predicate. Other `.apply()` methods on user classes don't match.
((call_expression
   function: (member_expression
     object: (identifier) @_obj
     property: (identifier) @_m)) @blind.function_apply
  (#eq? @_obj "Function")
  (#eq? @_m "apply"))
