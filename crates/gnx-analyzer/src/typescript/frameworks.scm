;; Framework-aware queries for TypeScript (Tier 1: Express subset).

;; Express: app.{get,post,put,delete,patch,use}(<path_str>, <handler_ident>)
;; Captures the handler identifier passed as second argument.
(call_expression
  function: (member_expression
    object: (identifier)
    property: (property_identifier) @express.route.method
    (#match? @express.route.method "^(get|post|put|delete|patch|use)$"))
  arguments: (arguments
    (string) @express.route.path
    (identifier) @express.route.handler))
