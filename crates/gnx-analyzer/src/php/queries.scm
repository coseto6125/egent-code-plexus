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
  name: (name) @method.name
  return_type: (_) @type.method ?) @method

;; Namespaces
(namespace_definition
  name: (namespace_name) @name.namespace) @namespace

;; Imports
(namespace_use_clause
  (_) @import.source
  alias: (use_as_clause (_) @import.alias)?) @import

(namespace_use_group
  (_) @import.prefix
  (namespace_use_clause
    (_) @import.source
    alias: (use_as_clause (_) @import.alias)?)) @import

;; Routes
(scoped_call_expression
  name: (identifier) @route.method (#match? @route.method "(?i)^(get|post|put|delete|patch)$")
  arguments: (arguments (string) @route.path)
) @route.call
