;; Module declarations — treat module as class-like entity
;; module_declaration > module_header > simple_identifier (module name)
(module_declaration
  (module_header
    (simple_identifier) @class.name)) @class

;; Module instantiations — first simple_identifier is the instantiated module type
;; e.g. `adder u1 (.a(x), .b(y));` — "adder" is the module being instantiated
(module_instantiation
  (simple_identifier) @import.source) @import

;; Function declarations inside modules
(function_declaration
  (function_body_declaration
    (function_identifier) @method.name)) @method

;; Task declarations inside modules
(task_declaration
  (task_body_declaration
    (task_identifier) @method.name)) @method

;; Named parameters (parameter WIDTH = 8)
(parameter_declaration
  (list_of_param_assignments
    (param_assignment
      (parameter_identifier) @const.name))) @const

;; Local parameters (localparam DEPTH = 16)
(local_parameter_declaration
  (list_of_param_assignments
    (param_assignment
      (parameter_identifier) @const.name))) @const
