;; Classes
(class
  name: [
    (constant)
    (scope_resolution)
  ] @name
  superclass: (superclass [ (constant) (scope_resolution) (identifier) ] @heritage)?
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
      (string_content) @route.path))
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
    [ (constant) (scope_resolution) ] @mixin_module))
