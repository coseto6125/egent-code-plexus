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
