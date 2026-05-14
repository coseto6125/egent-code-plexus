;; Classes
(class
  name: [
    (constant)
    (scope_resolution)
  ] @name.class
  superclass: (superclass)? @heritage) @class

;; Modules
(module
  name: [
    (constant)
    (scope_resolution)
  ] @name.module) @module

;; Methods
(method
  name: [
    (identifier)
    (constant)
    (operator)
    (setter)
  ] @name.method) @method

(singleton_method
  name: [
    (identifier)
    (constant)
    (operator)
    (setter)
  ] @name.method) @method

;; Requires
(call
  method: (identifier) @_require_call
  (#match? @_require_call "^(require|require_relative)$")
  arguments: (argument_list
    (string
      (string_content) @import.name))) @import
