;; Functions
(function_definition
  name: (name) @name.function
  return_type: (_) @type.function ?) @function

;; Classes
(class_declaration
  (modifier_list (visibility_modifier) @export)?
  name: (name) @name.class
  (base_clause (qualified_name) @heritage)?
  (class_interface_clause (qualified_name_list (qualified_name) @heritage))?) @class

;; Interfaces
(interface_declaration
  name: (name) @name.interface
  (interface_extends_clause (qualified_name_list (qualified_name) @heritage))?) @interface

;; Methods
(method_declaration
  (method_modifiers (visibility_modifier) @export)?
  name: (name) @name.method
  return_type: (_) @type.method ?) @method

;; Namespaces
(namespace_definition
  name: (namespace_name) @name.namespace) @namespace

;; Imports
(namespace_use_clause
  name: (qualified_name) @import.source
  alias: (use_as_clause (name) @import.alias)?) @import

(namespace_use_group
  prefix: (qualified_name) @import.prefix
  (namespace_use_clause
    name: (qualified_name) @import.source
    alias: (use_as_clause (name) @import.alias)?)) @import
