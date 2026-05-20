;; Classes
(class
  name: [
    (constant)
    (scope_resolution)
  ] @name
  superclass: (superclass (expression) @heritage)?
) @class

;; Modules
(module
  name: [
    (constant)
    (scope_resolution)
  ] @name
) @module

;; Methods
(method
  name: [
    (identifier)
    (constant)
    (operator)
    (setter)
  ] @name
) @method

(singleton_method
  name: [
    (identifier)
    (constant)
    (operator)
    (setter)
  ] @name
) @method

;; Requires
(call
  method: (identifier) @_require_call
  (#match? @_require_call "^(require|require_relative)$")
  arguments: (argument_list
    (string
      (string_content) @import.name))) @import

;; Routes
(call
  method: (identifier) @route.method
  (#match? @route.method "^(get|post|put|delete|patch|options)$")
  arguments: (argument_list
    (string
      [(string_content) @route.path (MISSING) @route.path]))
) @route

;; attr_reader / attr_writer / attr_accessor metaprogramming
;; Each symbol argument declares an instance property.
(call
  method: (identifier) @attr_kind
  (#match? @attr_kind "^attr_(reader|writer|accessor)$")
  arguments: (argument_list) @attr_args)

;; Mixins: include / extend ModuleName inside a class body
;; The mixin module constant gets appended to the enclosing class's heritage.
(call
  method: (identifier) @include_kind
  (#match? @include_kind "^(include|extend)$")
  arguments: (argument_list
    (expression) @mixin_module))

;; `alias new_name old_name` keyword — emits a named binding.
;; tree-sitter-ruby labels the NEW name as field `name` and the original as `alias`.
(alias
  name: (identifier) @alias.new
  alias: (identifier) @alias.old)

;; `alias_method :new_name, :old_name` metaprogramming — same shape as the
;; keyword form, but parsed as a regular `call`. The two `simple_symbol`
;; positional args carry the new and old names respectively.
(call
  method: (identifier) @_alias_method_call
  (#match? @_alias_method_call "^alias_method$")
  arguments: (argument_list) @alias_method.args)

;; Constant alias: `MyConst = OtherModule::Const` (or `MyConst = OtherConst`).
;; The lhs constraint to `(constant)` filters out `local_var = …` because
;; lowercase identifiers parse as `identifier`, not `constant`.
(assignment
  left: (constant) @const_alias.new
  right: (expression) @const_alias.source)

;; Constant declarations — any `UPPERCASE_NAME = <value>` assignment. lhs
;; must be `(constant)` (uppercase Ruby identifier); rhs is unconstrained,
;; so this catches every form: hash literals (`DEFAULT_OPTIONS = {...}`),
;; integers (`TOKEN_LENGTH = 32`), regex (`PORT_REGEXP = /:\d+\z/.freeze`),
;; strings, arrays of symbols (`DIRECTIVES = %i[...]`), etc. Real class-body
;; constants in Rails-style projects (CSRF tokens, dispatcher tables, …).
;;
;; This query overlaps with `const_alias` above when rhs is also a
;; constant — that's intentional: const_alias emits an alias binding for
;; FQN resolution, this emits the Const node itself. Different graph
;; purposes, no double-emit risk.
(assignment
  left: (constant) @name) @const

;; `def_delegator :target, :method` / `def_delegators :target, :m1, :m2, ...` /
;; `delegate :m1, :m2, to: :target` metaprogramming — each delegated method
;; becomes a named binding `<host>.<method>` aliased to `<target>.<method>`.
;; Receiver-awareness (only honour these when the enclosing class has
;; `extend Forwardable`) is done in `parser.rs` against `pending_mixins`;
;; the bare whitelist here is a known false-positive vector for user-defined
;; methods of the same name (documented in the named-binding spec).
(call
  method: (identifier) @delegator_method
  (#match? @delegator_method "^(def_delegator|def_delegators|delegate)$")
  arguments: (argument_list) @delegator_args)
