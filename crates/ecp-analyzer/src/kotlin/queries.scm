; Imports
(import_header
  (identifier) @import.source
  (import_alias (type_identifier) @alias)?) @import

; Classes
(class_declaration
  (modifiers
    (annotation)* @decorator
  )? @export
  (type_identifier) @class.name
  (delegation_specifier
      [
        (user_type (type_identifier) @heritage)
        (constructor_invocation (user_type (type_identifier) @heritage))
      ]
    )*
) @class

; Objects
(object_declaration
  (modifiers
    (annotation)* @decorator
  )? @export
  (type_identifier) @class.name
  (delegation_specifier
      [
        (user_type (type_identifier) @heritage)
        (constructor_invocation (user_type (type_identifier) @heritage))
      ]
    )*
) @class

; Primary constructors — class Foo(val x: Int) form.
; primary_constructor only appears in the tree when explicit params are present
; (class Foo without parens has no primary_constructor child), so this pattern
; does not over-emit for bare class declarations.
(class_declaration
  (type_identifier) @constructor.name
  (primary_constructor) @constructor)

; Secondary constructors — explicit constructor(...) blocks inside class_body.
; The name is implicit = enclosing class's type_identifier (same as primary).
(class_declaration
  (type_identifier) @constructor.name
  (class_body
    (secondary_constructor) @constructor))

; Functions
(function_declaration
  (modifiers
    (annotation)* @decorator
  )? @export
  (simple_identifier) @function.name
  (user_type)? @type) @function

; Properties — class-scoped only (val/var inside class_body).
; Top-level file-scoped `val`/`var` are excluded here (those belong to Variable round).
(class_body
  (property_declaration
    (variable_declaration
      (simple_identifier) @property.name)
  ) @property)

; Variables — top-level val/var (direct child of source_file only).
; Anchored to source_file so class-body property_declarations don't produce
; spurious duplicate Variable nodes alongside the @property capture above.
(source_file
  (property_declaration
    (variable_declaration
      (simple_identifier) @variable.name)
  ) @variable)

; Enum entries — `enum class X { A, B, C }` produces an `enum_entry` per
; identifier inside `enum_class_body`. Spec maps `enum_entry.name` to
; NodeKind::Enum so each entry surfaces as its own Enum node (mirrors
; ref-gitnexus convention). The parent enum class is captured separately
; via `class.name` + `is_enum_class` promotion at parser.rs:282.
(enum_class_body
  (enum_entry
    (simple_identifier) @enum_entry.name) @enum_entry)

; Override marker — `override fun foo()`. Kotlin REQUIRES the `override`
; keyword; its absence on a method that shadows a supertype member is a
; compile error. Captured separately so the post-process override resolver
; can identify candidate overriders without re-reading source text.
(function_declaration
  (modifiers
    (member_modifier) @override_marker
    (#eq? @override_marker "override"))
  (simple_identifier) @function.name) @function

; Type aliases — `typealias Callback = (String) -> Unit`.
; WHY: type aliases are real reference targets; `ecp find Callback` must resolve
; without grep, and impact queries need the Typedef→Function edges.
; tree-sitter-kotlin grammar: type_alias node contains (type_identifier) as the
; alias name (grammar.js: `alias($.simple_identifier, $.type_identifier)`).
(type_alias
  (type_identifier) @typedef.name) @typedef

; Anonymous callbacks: trailing lambda (`list.forEach { process(it) }`) and
; paren-position lambda (`list.map({ x -> f(x) })`). Without a node here their
; body's calls are dropped by attach_to_enclosing when no named enclosing scope
; exists — filter (A) callback registration. parser.rs only emits a node when
; the body contains a call, so empty lambdas add no bloat.
(call_suffix
  (annotated_lambda
    (lambda_literal) @function.anonymous))

(value_argument
  (lambda_literal) @function.anonymous)
