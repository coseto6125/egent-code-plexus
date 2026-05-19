;; Framework-aware queries for Rust (Tier 1: Axum router subset).

;; Axum: .route("/path", METHOD(handler_ident))
;; Captures the handler identifier passed as argument to a method call (get/post/put/delete/patch)
;; that is itself the second argument to .route(...).
(call_expression
  function: (field_expression
    field: (field_identifier) @_route (#eq? @_route "route"))
  arguments: (arguments
    (string_literal) @axum.route.path
    (call_expression
      function: (identifier) @axum.route.method
      arguments: (arguments
        (identifier) @axum.route.handler))))

;; Actix: #[get("/path")] / #[post(...)] / #[put] / #[delete] / #[patch] / #[head] on a fn.
;; Matches an attribute_item whose attribute's path identifier is an HTTP verb,
;; immediately followed by a function_item; captures the verb and the function name.
(_
  (attribute_item
    (attribute
      (identifier) @actix.route.method
      (#match? @actix.route.method "^(get|post|put|delete|patch|head)$")))
  .
  (function_item
    name: (identifier) @actix.route.handler))
