; Tables captured as class-like entities.
; CREATE PROCEDURE is not supported by tree-sitter-sequel grammar (PostgreSQL/ANSI dialect).
(create_table
  (object_reference (identifier) @class.name)) @class

; Views are named aliases for queries — emit as Typedef.
(create_view
  (object_reference (identifier) @typedef.name)) @typedef

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

; Foreign-key targets become Heritage entries on the referencing table.
; Pattern 1: inline column-level FK (`col INT REFERENCES other(id)`).
(create_table
  (object_reference (identifier) @heritage.table)
  (column_definitions
    (column_definition
      (keyword_references)
      (object_reference (identifier) @heritage.target))))

; Pattern 2: table-level FK, named or unnamed
; (`FOREIGN KEY (col) REFERENCES other(id)` and
;  `CONSTRAINT fk FOREIGN KEY (col) REFERENCES other(id)`).
; The named form parses with an ERROR node for `CONSTRAINT <ident> FOREIGN`,
; but the surviving `constraint` node still carries keyword_references +
; object_reference, so the same query catches both.
(create_table
  (object_reference (identifier) @heritage.table)
  (column_definitions
    (constraints
      (constraint
        (keyword_references)
        (object_reference (identifier) @heritage.target)))))
