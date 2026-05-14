;; Function definitions — both forms: `function foo() {}` and `foo() {}`
(function_definition
  name: (word) @function.name) @function

;; Source imports: `source ./lib.sh` and `. ./lib.sh`
(command
  name: (command_name) @_cmd
  argument: (word) @import.source
  (#eq? @_cmd "source")) @import

(command
  name: (command_name) @_cmd
  argument: (word) @import.source
  (#eq? @_cmd ".")) @import

;; Top-level variable assignments
(variable_assignment
  name: (variable_name) @const.name) @const
