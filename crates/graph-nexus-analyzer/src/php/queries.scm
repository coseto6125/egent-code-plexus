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
  name: (_) @route.method (#match? @route.method "(?i)^(get|post|put|delete|patch)$")
  arguments: (_) @route.path
) @route.call

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
