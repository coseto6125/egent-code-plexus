; Tables and views captured as class-like entities.
; CREATE PROCEDURE is not supported by tree-sitter-sequel grammar (PostgreSQL/ANSI dialect).
(create_table
  (object_reference (identifier) @class.name)) @class

(create_view
  (object_reference (identifier) @class.name)) @class

; Functions
(create_function
  (object_reference (identifier) @function.name)) @function

; Column field names within table definitions.
; Uses field-name selector `name:` to target only the column identifier,
; not identifiers inside REFERENCES(...) clauses.
(column_definition
  name: (identifier) @const.name) @const

; Foreign key references become import-style edges.
(column_definition
  (keyword_references)
  (object_reference (identifier) @import.source))
