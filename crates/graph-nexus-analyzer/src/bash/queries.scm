;; Function definitions — both forms: `function foo() {}` and `foo() {}`
(function_definition
  name: (word) @function.name) @function

;; Source imports: `source ./lib.sh` and `. ./lib.sh`
;; Unquoted argument
(command
  name: (command_name) @_cmd
  argument: (word) @import.source
  (#match? @_cmd "^(source|\\.)$")) @import

;; Double-quoted argument: `source "lib.sh"` — capture inner content (no quotes)
(command
  name: (command_name) @_cmd
  argument: (string (string_content) @import.source)
  (#match? @_cmd "^(source|\\.)$")) @import

;; Single-quoted argument: `source 'lib.sh'` — captures raw_string including quotes;
;; parser strips the surrounding single quotes.
(command
  name: (command_name) @_cmd
  argument: (raw_string) @import.source
  (#match? @_cmd "^(source|\\.)$")) @import

;; Top-level variable assignments
(variable_assignment
  name: (variable_name) @const.name) @const
