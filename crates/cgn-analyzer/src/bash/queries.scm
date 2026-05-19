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

;; Shell aliases: `alias NAME=...`
;; Two forms:
;;   1. Quoted/mixed RHS: argument is a `concatenation` whose first word holds "NAME=".
;;   2. Unquoted RHS: argument is a bare `word` with text "NAME=value".
;; In both cases @typedef.raw captures the word containing "NAME="; parser.rs strips
;; everything from `=` onward (including the value) to extract just the alias name.
(command
  name: (command_name) @_alias_cmd
  argument: (concatenation
    (word) @typedef.raw)
  (#eq? @_alias_cmd "alias")) @typedef

(command
  name: (command_name) @_alias_cmd2
  argument: (word) @typedef.raw
  (#eq? @_alias_cmd2 "alias")) @typedef
