;; Classes
(class_declaration
  (attribute_list)* @decorator
  (modifier)* @export
  name: (identifier) @name.class
  (base_list (_)* @heritage)?
) @class

;; Structs
(struct_declaration
  (attribute_list)* @decorator
  (modifier)* @export
  name: (identifier) @name.class
  (base_list (_)* @heritage)?
) @class

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
  name: (identifier) @name.class
  (base_list (_)* @heritage)?
) @class

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

;; Constructors
(constructor_declaration
  (attribute_list)* @decorator
  (modifier)* @export
  name: (identifier) @name.constructor
) @constructor

;; Local Functions
(local_function_statement
  (attribute_list)* @decorator
  (modifier)* @export
  returns: (_) @type
  name: (identifier) @name.function
) @function

;; Using directives (Imports)
(using_directive
  name: (_) @import.name @import.source
) @import

(using_directive
  name: (identifier) @import.alias
  (_) @import.name @import.source
) @import
