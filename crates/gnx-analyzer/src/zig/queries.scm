;; @import via builtin_function — Zig uses @import("path") not a keyword
;; The predicate filters builtin_function nodes to only those named @import
(variable_declaration
  (builtin_function
    (builtin_identifier) @_builtin_name
    (arguments
      (string
        (string_content) @import.source)))
  (#eq? @_builtin_name "@import")) @import

;; Function declarations (top-level and nested in structs)
(function_declaration
  name: (identifier) @function.name) @function

;; Struct declarations — Zig encodes structs as `const Name = struct { ... };`
(variable_declaration
  (identifier) @struct.name
  (struct_declaration)) @struct

;; Top-level const / var declarations (scalar values, not structs or imports)
;; Processed in Rust: skipped when the same declaration matches @import or @struct
(variable_declaration
  (identifier) @const.name) @const
