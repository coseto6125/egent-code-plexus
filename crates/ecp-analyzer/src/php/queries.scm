;; Functions
(function_definition
  (attribute_list)* @decorator
  name: (name) @name.function
  return_type: (_) @type.function ?) @function

;; Classes
(class_declaration
  (attribute_list)* @decorator
  (visibility_modifier)? @export
  name: (name) @name.class
  (base_clause (name) @heritage)?
  (class_interface_clause (name) @heritage)?) @class

;; Interfaces
(interface_declaration
  (attribute_list)* @decorator
  name: (name) @name.interface
  (base_clause (name) @heritage)?) @interface

;; Methods
(method_declaration
  (attribute_list)* @decorator
  (visibility_modifier)? @export
  name: (name) @name.method
  return_type: (_) @type.method ?) @method

;; Properties — regular class property declarations (`public int $foo;`)
;; One Property node per property_element so `$x, $y;` emits two nodes.
(property_declaration
  (visibility_modifier)? @export
  (property_element
    name: (variable_name (name) @name.property))) @property

;; Properties — PHP 8.0+ constructor promotion (`public string $name`)
;; `visibility` field is always present; the `$` prefix is anonymous.
(property_promotion_parameter
  visibility: (visibility_modifier) @export
  name: (variable_name (name) @name.property)) @property

;; Namespaces
(namespace_definition
  name: (namespace_name) @name.namespace) @namespace

;; Traits (PHP 5.4+)
(trait_declaration
  name: (name) @name.trait) @trait

;; Enums (PHP 8.1+)
(enum_declaration
  name: (name) @name.enum) @enum

;; Class / interface / module constants — `const NAME = ...`. The capture
;; anchors on the inner `const_element` (one per declared name) so
;; `const A = 1, B = 2;` emits two Const nodes rather than one for the
;; whole declaration. Covers both class-body constants (the common case —
;; Laravel `CREATED_AT`, `INVALID_TOKEN`, etc.) and module-level
;; `const X = ...` outside any class.
(const_declaration
  (const_element (name) @name.const) @const)

;; Imports
(namespace_use_clause
  (_) @import.source
  alias: (use_as_clause (_) @import.alias)?) @import

(namespace_use_group
  (_) @import.prefix
  (namespace_use_clause
    (_) @import.source
    alias: (use_as_clause (_) @import.alias)?)) @import

;; Routes — capture scope (class name) + first string argument so the parser
;; can both gate emission on a router-class allowlist (skip `Cache::get`,
;; `Config::get`, `Auth::get`, etc.) and store a clean path string rather
;; than the entire arguments node. Laravel paths can be bare (`'register'`),
;; absolute (`'/users'`), or contain params (`'users/{id}'`); all valid
;; structurally — the scope gate is what filters out non-route facades.
(scoped_call_expression
  scope: (name) @route.scope
  name: (name) @route.method (#match? @route.method "(?i)^(get|post|put|delete|patch)$")
  arguments: (arguments . (argument [(string) (encapsed_string) (MISSING)] @route.path))
) @route.call

;; Chained-call routes — `Route::middleware(['auth'])->get('/path', ...)`,
;; `Route::middleware(...)->prefix(...)->post('/x', ...)`. Catches the same
;; HTTP-verb call as above but expressed as a member call chained off a
;; scoped_call_expression. The parser walks `route.chained.object` inward
;; through any depth of `member_call_expression` and verifies the root is
;; a `scoped_call_expression` with a router-allowlist scope.
(member_call_expression
  object: (_) @route.chained.object
  name: (name) @route.method (#match? @route.method "(?i)^(get|post|put|delete|patch)$")
  arguments: (arguments . (argument [(string) (encapsed_string) (MISSING)] @route.path))
) @route.chained.call

;; ---- Laravel ----
;; `Route::<method>('/path', <handler>)`. Mirrors upstream
;; `gitnexus/src/core/group/extractors/http-patterns/php.ts:34-42`. The
;; outer call is the only structural anchor; the parser walks the
;; `arguments` node at parse time to extract path + handler shape.
;; Gated downstream by `use Illuminate\...`.
(scoped_call_expression
  scope: (name) @_laravel_route_class (#eq? @_laravel_route_class "Route")
  name: (name) @laravel.route.method
    (#match? @laravel.route.method "^(get|post|put|patch|delete|options|any)$")
  arguments: (arguments) @laravel.route.args) @laravel.route.call

;; ---- BlindSpot patterns (FU-001 P5a) ----
;; eval(<expr>) — runtime PHP code execution. PHP `eval` is a language
;; construct, not a function — tree-sitter-php emits `(include_expression)`
;; for `include/require` and a dedicated `(eval_intrinsic)` (or similar)
;; node — but the call_user_func-style match here works on most grammar
;; versions where eval appears as a function_call_expression.
((function_call_expression
   function: (name) @_fn) @blind.eval
  (#eq? @_fn "eval"))

;; call_user_func(<callable>, ...) — variable-callable dispatch. The parser
;; gates emission on the first argument being non-literal (per Constraint 2);
;; `call_user_func('strlen', ...)` is statically resolvable and skipped.
((function_call_expression
   function: (name) @_fn) @blind.call_user_func
  (#eq? @_fn "call_user_func"))

;; $<var>(...) — variable function call. PHP allows calling any variable
;; that holds a callable, so the target is bound at runtime via the
;; variable value.
((function_call_expression
   function: (variable_name)) @blind.variable_call)
