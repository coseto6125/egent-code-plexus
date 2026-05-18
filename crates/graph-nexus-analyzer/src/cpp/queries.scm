;; Classes
(class_specifier
  name: [
    (type_identifier)
    (template_type)
  ] @name.class
  (base_class_clause
    (_ (type_identifier) @heritage))?
) @class

;; Structs — emitted as NodeKind::Struct (distinct from Class)
(struct_specifier
  name: [
    (type_identifier)
    (template_type)
  ] @name.struct
  (base_class_clause
    (_ (type_identifier) @heritage))?
) @struct

;; Functions
;;
;; Two outer shapes for return type:
;;   `int foo()`               → declarator = function_declarator(...)
;;   `int* foo()` / `int& foo()` → declarator = pointer/reference_declarator
;;                                            wrapping function_declarator
;; Tree-sitter-cpp folds the `*` / `&` into the declarator chain rather than
;; the type, so the outer wrapper must be matched explicitly.
(function_definition
  type: (_)? @type
  declarator: [
    (function_declarator
      declarator: [
        (identifier) @name.function
        (reference_declarator (identifier) @name.function)
        (pointer_declarator (identifier) @name.function)
      ])
    (pointer_declarator
      (function_declarator
        declarator: (identifier) @name.function))
    (reference_declarator
      (function_declarator
        declarator: (identifier) @name.function))
    (pointer_declarator
      (pointer_declarator
        (function_declarator
          declarator: (identifier) @name.function)))
  ]
) @function

;; Free function declarations (prototypes, no body). Matches `int f();`,
;; `std::map<X,Y> getAll();`, `void* alloc(size_t n);` so the return-type
;; annotation lands on a Function node when the .cpp/.h split puts the
;; prototype in a header.
;;
;; No outer parent anchor (was `(translation_unit ...)` before): real
;; `.h` files wrap declarations in `#ifndef X / #define X / ... / #endif`
;; header guards (every header file does this) AND/OR `extern "C" { ... }`
;; AND/OR `namespace foo { ... }`. tree-sitter-cpp parses each wrapper as
;; an intermediate node — `preproc_ifdef`, `linkage_specification`,
;; `namespace_definition` — that broke the previous translation_unit
;; anchor. Match anywhere; let parser.rs `is_inline_class_member` walk
;; the parent chain to promote `Function` → `Method` when the declaration
;; is inside a `field_declaration_list` (class body). The walker stops at
;; `translation_unit | namespace_definition | linkage_specification`, so
;; namespace / extern "C" / TU-level decls stay `Function`.
(declaration
  type: (_) @type
  declarator: [
    (function_declarator
      declarator: [
        (identifier) @name.function
        (reference_declarator (identifier) @name.function)
        (pointer_declarator (identifier) @name.function)
      ])
    (pointer_declarator
      (function_declarator
        declarator: (identifier) @name.function))
    (reference_declarator
      (function_declarator
        declarator: (identifier) @name.function))
    (pointer_declarator
      (pointer_declarator
        (function_declarator
          declarator: (identifier) @name.function)))
  ]
) @function

;; Methods
;;
;; Same outer-wrapper expansion as Functions, plus the qualified_identifier
;; branch for out-of-class definitions like `int* Foo::bar() { ... }`.
(function_definition
  type: (_)? @type
  declarator: [
    (function_declarator
      declarator: [
        (field_identifier) @name.method
        (reference_declarator (field_identifier) @name.method)
        (pointer_declarator (field_identifier) @name.method)
        (qualified_identifier
          name: [
            (identifier)
            (field_identifier)
          ] @name.method
        )
        (reference_declarator (qualified_identifier name: (_) @name.method))
        (pointer_declarator (qualified_identifier name: (_) @name.method))
      ])
    (pointer_declarator
      (function_declarator
        declarator: [
          (field_identifier) @name.method
          (qualified_identifier name: (_) @name.method)
        ]))
    (reference_declarator
      (function_declarator
        declarator: [
          (field_identifier) @name.method
          (qualified_identifier name: (_) @name.method)
        ]))
    (pointer_declarator
      (pointer_declarator
        (function_declarator
          declarator: [
            (field_identifier) @name.method
            (qualified_identifier name: (_) @name.method)
          ])))
  ]
) @method

;; Member function declarations inside a class / struct body — `int sum();`.
;; Distinct from `function_definition` (no body) so a separate match emits a
;; Method node with the return-type capture.
(field_declaration
  type: (_) @type
  declarator: [
    (function_declarator
      declarator: [
        (field_identifier) @name.method
        (pointer_declarator (field_identifier) @name.method)
        (reference_declarator (field_identifier) @name.method)
      ])
    (pointer_declarator
      (function_declarator
        declarator: (field_identifier) @name.method))
    (reference_declarator
      (function_declarator
        declarator: (field_identifier) @name.method))
    (pointer_declarator
      (pointer_declarator
        (function_declarator
          declarator: (field_identifier) @name.method)))
  ]
) @method

;; Deleted / defaulted / bodied operator= and destructor methods.
;; Tree-sitter represents `operator=(T&) = delete;` and `~Foo() = default;`
;; as `function_definition` whose declarator chain ends at `operator_name` or
;; `destructor_name`.  The common pattern is:
;;   function_definition
;;     (reference_declarator          ← `T& operator=`
;;       (function_declarator
;;         declarator: (operator_name)))
;;   function_definition
;;     (function_declarator           ← `~Foo()`
;;       declarator: (destructor_name))
(function_definition
  declarator: [
    (function_declarator
      declarator: [
        (operator_name) @name.method
        (destructor_name) @name.method
      ])
    (reference_declarator
      (function_declarator
        declarator: (operator_name) @name.method))
    (pointer_declarator
      (function_declarator
        declarator: (operator_name) @name.method))
  ]
) @method


;; Preprocessor Includes
(preproc_include
  path: [
    (string_literal)
    (system_lib_string)
  ] @import.source
) @import

;; Namespace Aliases
(namespace_alias_definition
  name: (namespace_identifier) @alias
  (namespace_identifier) @import.source
) @import

;; Function parameters — `int x` / `const std::string& s` / `std::vector<int> v`.
;; Captures the outer `parameter_declaration` + the parameter's identifier.
;; The parser slices the source between [decl.start, name.start) to preserve
;; the full type text including templates, qualifiers, and `*` / `&` ops.
(parameter_declaration
  declarator: [
    (identifier) @param.name
    (pointer_declarator (identifier) @param.name)
    (pointer_declarator (pointer_declarator (identifier) @param.name))
    (reference_declarator (identifier) @param.name)
    (array_declarator declarator: (identifier) @param.name)
  ]) @param

;; Class / struct data-member declarations — `int x;` / `std::string name;`.
;; Restricted to declarators that end at a plain `field_identifier` so
;; member-function declarations (whose declarator is `function_declarator`)
;; don't match here — those are captured by the `@method` rule above.
(field_declaration
  declarator: [
    (field_identifier) @field.name
    (pointer_declarator (field_identifier) @field.name)
    (pointer_declarator (pointer_declarator (field_identifier) @field.name))
    (reference_declarator (field_identifier) @field.name)
    (array_declarator declarator: (field_identifier) @field.name)
  ]) @field

;; Top-level variable / const declarations — `auto x = 5;` / `int N = 5;`.
;; The parser slices [decl.start, name.start), so `auto x` yields `"auto"`
;; (no deduced type — that requires semantic analysis the analyzer
;; doesn't perform). Storage-class and qualifier words are preserved.
(translation_unit
  (declaration
    declarator: [
      (init_declarator declarator: (identifier) @var.name)
      (init_declarator declarator: (pointer_declarator (identifier) @var.name))
      (init_declarator declarator: (reference_declarator (identifier) @var.name))
      (identifier) @var.name
      (pointer_declarator (identifier) @var.name)
    ]) @var)

;; Preprocessor macro definitions — `#define NAME ...` and `#define F(x) ...`
(preproc_def
  name: (identifier) @name.macro
) @macro

(preproc_function_def
  name: (identifier) @name.macro
) @macro

;; Namespace definitions — `namespace foo { ... }`
(namespace_definition
  name: [
    (namespace_identifier) @name.namespace
    (nested_namespace_specifier) @name.namespace
  ]
) @namespace

;; Enum definitions — `enum class Color { ... }` and `enum OldEnum { ... }`
(enum_specifier
  name: (type_identifier) @name.enum
) @enum_node

;; Type aliases — `using Foo = Bar;`  and  `typedef int MyInt;`
(alias_declaration
  name: (type_identifier) @name.typedef
) @typedef_node

(type_definition
  declarator: (type_identifier) @name.typedef
) @typedef_node
