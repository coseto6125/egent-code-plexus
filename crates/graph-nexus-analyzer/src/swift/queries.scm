;; Declarations — class/struct/enum share `class_declaration` in tree-sitter-swift;
;; kind is disambiguated in parser.rs via swift_decl_keyword().
(class_declaration
  (modifiers
    [
      (visibility_modifier) @export
      (attribute) @decorator
    ]*
  )?
  name: [
    (type_identifier)
    (user_type (type_identifier))
  ] @class.name
  (inheritance_specifier inherits_from: (user_type (type_identifier) @heritage))?
) @class

;; ERROR-recovery fallback for `#if`/`#endif` directives near a class header.
;; tree-sitter 0.25's ERROR-recovery (more aggressive than 0.21 which ref-gitnexus
;; pins) swallows the class header when a conditional-compilation directive
;; sits where the grammar does not allow it. Two observed shapes:
;;
;;   Shape 1 — `#if` *inside* class_body: the entire `class Foo: Bar { ... }`
;;   header collapses into a flat ERROR directly under source_file. The ERROR
;;   keeps `modifiers` + `"class"` keyword + `simple_identifier` (the class
;;   name, not as a `type_identifier` because class_declaration framing is gone).
;;
;;   Shape 2 — `#if` *outside* class_body (file-level `#if canImport(_Concurrency)`
;;   wrapping the entire class). Recovery preserves `"class"` keyword as a sibling
;;   of a nested `ERROR` that contains the `simple_identifier` (class name); the
;;   wrapping node is typically `function_declaration` (tree-sitter's cost-based
;;   search picks function_declaration when the file body is large).
;;
;; Names are captured as `@class.name`; `@class` root is the ERROR-or-wrapper
;; node so `swift_decl_keyword()` walks its children to find the leading
;; "class"/"struct"/"enum" keyword (still works in both shapes since the
;; keyword token is preserved). Heritage cannot be reliably recovered (wrapped
;; in nested ERROR), so this alternation deliberately omits the heritage
;; capture.
(ERROR
  "class"
  (simple_identifier) @class.name) @class

(_
  "class"
  (ERROR
    (simple_identifier) @class.name)) @class

;; Swift `protocol P {}` → Trait (distinct from Java/C# Interface).
(protocol_declaration
  (modifiers (visibility_modifier) @export)?
  name: (type_identifier) @trait.name) @trait

;; Functions — `func f(...) -> Bool`.
(function_declaration
  (modifiers (visibility_modifier) @export)?
  name: (simple_identifier) @function.name) @function

;; Function parameters — `name: Type`.
(parameter
  (simple_identifier) @param.name
  (user_type (type_identifier) @param.type)) @param

;; Property declarations — `var x: Int` / `var x = 0` / `let (a,b) = ...`.
;; One match per property_declaration. parser.rs walks @property.name.pat
;; (the pattern node) to collect all simple_identifier leaves (handles both
;; simple `var x` and tuple `let (a,b)` bindings), and walks the
;; property_declaration node's direct children to find any type_annotation.
(property_declaration
  (pattern) @property.name.pat) @property

;; Enum cases — `case foo` / `case bar(Int)` / `case a, b, c`. Each
;; `simple_identifier` inside `enum_entry` is a separately-named case;
;; multi-name `case a, b, c` produces three captures. parser.rs emits
;; one Property node per case name (always type-level — no scope walk
;; needed, an `enum_entry` only ever lives inside `enum_class_body`).
(enum_entry
  (simple_identifier) @enum_case.name) @enum_case

;; Constructors — Swift `init(...)` is a distinct `init_declaration` node.
(init_declaration) @constructor

;; Imports — Swift `import Module`.
(import_declaration
  (identifier (simple_identifier) @import.name @import.source)
) @import

;; Typealias declarations — `typealias MyInt = Int` or generic
;; `typealias R<T> = Swift.Result<T, Error>`. Captured at the top level so the
;; parser can read lhs name + full rhs text (including generics) from the
;; @typealias node's byte range.
(typealias_declaration) @typealias
