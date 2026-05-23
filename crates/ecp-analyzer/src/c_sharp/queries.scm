;; Classes
(class_declaration
  (attribute_list)* @decorator
  (modifier)* @export
  name: (identifier) @name.class
  (base_list (_)* @heritage)?
) @class

;; Structs — emitted as NodeKind::Struct (value-type aggregate, distinct
;; from Class because runtime semantics differ: value-copy, no inheritance).
(struct_declaration
  (attribute_list)* @decorator
  (modifier)* @export
  name: (identifier) @struct.name
  (base_list (_)* @heritage)?
) @struct

;; Interfaces
(interface_declaration
  (attribute_list)* @decorator
  (modifier)* @export
  name: (identifier) @name.interface
  (base_list (_)* @heritage)?
) @interface

;; Enums
(enum_declaration
  (attribute_list)* @decorator
  (modifier)* @export
  name: (identifier) @enum.name
  (base_list (_)* @heritage)?
) @enum

;; Enum members — `enum Status { Active = 0, Inactive = 1 }`
(enum_declaration
  body: (enum_member_declaration_list
    (enum_member_declaration
      name: (identifier) @enum_member.name) @enum_member_node))

;; Records
(record_declaration
  (attribute_list)* @decorator
  (modifier)* @export
  name: (identifier) @name.class
  (base_list (_)* @heritage)?
) @class

;; Methods
(method_declaration
  (attribute_list)* @decorator
  (modifier)* @export
  returns: (_) @type
  name: (identifier) @name.method
) @method

;; Override marker on methods — `override void Foo()`. C# requires the
;; `override` modifier to be explicit; absence means no override.
(method_declaration
  (modifier) @override_marker
  (#eq? @override_marker "override")
  name: (identifier) @name.method
) @method

;; Constructors — distinct NodeKind::Constructor
(constructor_declaration
  (attribute_list)* @decorator
  (modifier)* @export
  name: (identifier) @constructor.name
) @constructor

;; Local Functions
(local_function_statement
  (attribute_list)* @decorator
  (modifier)* @export
  returns: (_) @type
  name: (identifier) @name.function
) @function

;; Delegates — `public delegate ReturnT Name(...);`. No `NodeKind::Delegate`
;; in ecp; emit as Function (closest semantic — a delegate IS a
;; function-pointer type alias). ref-gitnexus uses a dedicated `Delegate`
;; label; the cross-side label mismatch is handled by the parity
;; aggregator's LABEL_PAIRS as Delegate↔Function.
(delegate_declaration
  (attribute_list)* @decorator
  (modifier)* @export
  returns: (_)? @type
  name: (identifier) @name.function
) @function

;; Fields (class-level variables) — one Property per declarator so
;; `private int x, y;` emits two Property nodes.
(field_declaration
  (attribute_list)* @decorator
  (modifier)* @export
  (variable_declaration
    (variable_declarator
      name: (identifier) @property.name))
) @property

;; Auto-properties and expression-bodied properties.
(property_declaration
  (attribute_list)* @decorator
  (modifier)* @export
  type: (_) @type
  name: (identifier) @property.name
) @property

;; Using directives (Imports). Three patterns:
;; - `using X;` / `using X.Y;` — plain
;; - `using static X.Alpha;` — static-member import (the `static` modifier
;;   is anonymous; the actual qualified-name child holds the path)
;; - `using A = X.Alpha;` — alias
;;
;; The `name:` field is unreliable across c_sharp grammar versions for
;; non-alias forms, so the plain/static patterns match the unnamed
;; qualified-name / identifier child directly.
(using_directive
  (qualified_name) @import.name @import.source
) @import

(using_directive
  (identifier) @import.name @import.source
) @import

(using_directive
  name: (identifier) @import.alias
  (_) @import.name @import.source
) @import

;; Namespaces (block-scoped and file-scoped / C# 10+)
(namespace_declaration
  name: (_) @namespace.name
) @namespace

(file_scoped_namespace_declaration
  name: (_) @namespace.name
) @namespace

;; ---- BlindSpot patterns (FU-001 P2c) ----
;; Activator.CreateInstance(<expr>) — runtime type instantiation. Receiver
;; constrained to direct identifier "Activator" to exclude `a.b.CreateInstance`.
((invocation_expression
   function: (member_access_expression
     expression: (identifier) @_obj
     name: (identifier) @_m)) @blind.activator_create
  (#eq? @_obj "Activator")
  (#eq? @_m "CreateInstance"))

;; <any>.Invoke(<args>) — reflective MethodInfo.Invoke or Delegate.Invoke.
;; Per Constraint 3 the outermost call in a chain
;; `t.GetMethod(name).Invoke(target, args)` is the dispatch site.
((invocation_expression
   function: (member_access_expression
     name: (identifier) @_m)) @blind.method_invoke
  (#eq? @_m "Invoke"))
