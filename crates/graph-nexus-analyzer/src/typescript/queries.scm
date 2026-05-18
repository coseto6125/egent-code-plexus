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

;; Constants — module-level only. Anchored to direct children of `program`;
;; function-body / block-scope `const x = …` declarations are intentionally
;; dropped (they bloat Const counts without LLM-disambiguation value).
;; The `export_statement` wrapper below already implies module scope.
(program
  (lexical_declaration
    (variable_declarator
      name: (identifier) @const.name
    )
  ) @const)

(export_statement
  declaration: (lexical_declaration
    (variable_declarator
      name: (identifier) @const.name
    ) @const
  )
) @export

;; Variables — module-level only (parallel to `lexical_declaration` above).
(program
  (variable_declaration
    (variable_declarator
      name: (identifier) @variable.name
    )
  ) @variable)

(export_statement
  declaration: (variable_declaration
    (variable_declarator
      name: (identifier) @variable.name
    ) @variable
  )
) @export

;; Classes — heritage lives inside class_heritage, not directly on class_declaration.
;; extends_clause carries `value: identifier`; implements_clause lists type_identifiers.
;; Both extend and implements are optional and can coexist in one class_heritage block.
(class_declaration
  (decorator)* @decorator
  name: (type_identifier) @class.name
  (class_heritage (extends_clause value: (identifier) @heritage))?
  (class_heritage (implements_clause (type_identifier) @heritage))?
) @class

(export_statement
  (class_declaration
    (decorator)* @decorator
    name: (type_identifier) @class.name
    (class_heritage (extends_clause value: (identifier) @heritage))?
    (class_heritage (implements_clause (type_identifier) @heritage))?
  ) @class
) @export

;; Constructors — method_definition named "constructor" is a distinct semantic.
;; Must come before the generic @method pattern so the span node is set to @constructor,
;; which maps to NodeKind::Constructor via spec.rs CAPTURE_KIND.
(method_definition
  name: (property_identifier) @constructor.name
  (#eq? @constructor.name "constructor")
) @constructor

;; Methods — class methods, interface method signatures, abstract method signatures.
;; The `(#not-eq?)` predicate prevents the generic @method pattern from also
;; firing on constructor method_definitions (which match @constructor above);
;; without it, every constructor produces both a Constructor and a Method node
;; for the same span, inflating gnx-rs Constructor counts ~25%.
(method_definition
  name: (property_identifier) @method.name
  return_type: (type_annotation (type_identifier) @type)?
  (#not-eq? @method.name "constructor")
) @method

(method_signature
  name: (property_identifier) @method.name
) @method

(abstract_method_signature
  name: (property_identifier) @method.name
) @method

;; Interfaces — extends uses extends_type_clause with repeated `type:` children.
(interface_declaration
  name: (type_identifier) @interface.name
  (extends_type_clause (type_identifier) @heritage)?
) @interface

(export_statement
  (interface_declaration
    name: (type_identifier) @interface.name
    (extends_type_clause (type_identifier) @heritage)?
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
