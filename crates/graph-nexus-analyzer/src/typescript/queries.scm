;; Functions
(function_declaration
  name: (identifier) @function.name
  return_type: (type_annotation (type_identifier) @type)?
) @function

(export_statement
  (function_declaration
    name: (identifier) @function.name
    return_type: (type_annotation (type_identifier) @type)?
  ) @function
) @export

;; Arrow Functions assigned to variables
(lexical_declaration
  (variable_declarator
    name: (identifier) @function.name
    value: (arrow_function)
  )
) @function

(export_statement
  declaration: (lexical_declaration
    (variable_declarator
      name: (identifier) @function.name
      value: (arrow_function)
    ) @function
  )
) @export

(variable_declaration
  (variable_declarator
    name: (identifier) @function.name
    value: (arrow_function)
  )
) @function

(export_statement
  declaration: (variable_declaration
    (variable_declarator
      name: (identifier) @function.name
      value: (arrow_function)
    ) @function
  )
) @export

;; Constants
(lexical_declaration
  (variable_declarator
    name: (identifier) @const.name
  )
) @const

(export_statement
  declaration: (lexical_declaration
    (variable_declarator
      name: (identifier) @const.name
    ) @const
  )
) @export

;; Variables
(variable_declaration
  (variable_declarator
    name: (identifier) @variable.name
  )
) @variable

(export_statement
  declaration: (variable_declaration
    (variable_declarator
      name: (identifier) @variable.name
    ) @variable
  )
) @export

;; Classes
(class_declaration
  (decorator)* @decorator
  name: (type_identifier) @class.name
  (extends_clause value: (identifier) @heritage)?
) @class

(export_statement
  (class_declaration
    (decorator)* @decorator
    name: (type_identifier) @class.name
    (extends_clause value: (identifier) @heritage)?
  ) @class
) @export

;; Methods — class methods, interface method signatures, abstract method signatures
(method_definition
  name: (property_identifier) @method.name
  return_type: (type_annotation (type_identifier) @type)?
) @method

(method_signature
  name: (property_identifier) @method.name
) @method

(abstract_method_signature
  name: (property_identifier) @method.name
) @method

;; Interfaces
(interface_declaration
  name: (type_identifier) @interface.name
  (extends_clause value: (identifier) @heritage)?
) @interface

(export_statement
  (interface_declaration
    name: (type_identifier) @interface.name
    (extends_clause value: (identifier) @heritage)?
  ) @interface
) @export

;; Properties — class fields (public_field_definition) and constructor parameter
;; properties (required_parameter / optional_parameter with accessibility modifier).
;; Interface property_signature is intentionally omitted: ref-gitnexus does not
;; emit those as Property nodes.
(public_field_definition
  name: (property_identifier) @property.name
) @property

(required_parameter
  (accessibility_modifier)
  pattern: (identifier) @property.name
) @property

(optional_parameter
  (accessibility_modifier)
  pattern: (identifier) @property.name
) @property

;; Imports (Named)
(import_statement
  (import_clause
    (named_imports
      (import_specifier
        name: (identifier) @import.name
        alias: (identifier)? @import.alias
      )
    )
  )
  source: (string (string_fragment) @import.source)
) @import

;; Imports (Default)
(import_statement
  (import_clause
    (identifier) @import.name
  )
  source: (string (string_fragment) @import.source)
) @import

;; Re-exports — `export { X as Y } from 'lib'` (and `export { X } from 'lib'`).
;; Captured separately from regular imports so the alias is preserved on the
;; emitted RawImport (parser sets `imported_name = X`, `alias = Some(Y)`).
(export_statement
  (export_clause
    (export_specifier
      name: (identifier) @import.name
      alias: (identifier)? @import.alias))
  source: (string (string_fragment) @import.source)) @import

;; Namespace re-export — `export * as ns from 'lib'`. The local namespace
;; binding `ns` is captured as the alias; `imported_name` is "*" (sentinel
;; matching the namespace import convention).
(export_statement
  (namespace_export
    (identifier) @import.alias)
  source: (string (string_fragment) @import.source)) @import.namespace

;; Type aliases
(type_alias_declaration
  name: (type_identifier) @typedef.name
) @typedef

(export_statement
  declaration: (type_alias_declaration
    name: (type_identifier) @typedef.name
  ) @typedef
) @export

;; Enums — plain `enum X`, `const enum X`, and `declare enum X` all share
;; `enum_declaration` as the parent node. The capture span is the inner
;; enum_declaration regardless of any `export`/`declare` wrapper.
(enum_declaration
  name: (identifier) @enum.name
) @enum

(export_statement
  (enum_declaration
    name: (identifier) @enum.name
  ) @enum
) @export

;; Routes — `app.METHOD(path, handler)` form.
;; `route.handler` captures the named handler argument when present so the
;; builder can emit a `HandlesRoute` edge from the handler function back
;; to the Route node. Inline / anonymous handlers (arrow fn, fn literal)
;; are not captured and the edge is skipped — the Route node still lands.
(call_expression
  function: (member_expression property: (property_identifier) @route.method (#match? @route.method "^(get|post|put|delete|patch|all|options|head|GET|POST|PUT|DELETE|PATCH)$"))
  arguments: (arguments
    (string (string_fragment) @route.path)
    (identifier)? @route.handler)
) @route.call
