;; Classes (superclass field is optional)
(class_def
  name: (constant) @class.name
  superclass: (_) @heritage
) @class

(class_def
  name: (constant) @class.name
  !superclass
) @class

;; Modules (treated as class-like for grouping)
(module_def
  name: (constant) @class.name
) @class

;; Methods
(method_def
  name: (identifier) @method.name
) @method

;; Require imports
(require
  (string
    (literal_content) @import.source)) @import

;; Top-level constants (UPPERCASE_NAMES = ...)
(const_assign
  lhs: (constant) @const.name
) @const
