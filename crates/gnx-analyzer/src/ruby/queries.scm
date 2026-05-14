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
