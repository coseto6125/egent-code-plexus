;; Framework-aware queries for Rust (Tier 1: Axum/Actix routes + Redis pub/sub).

;; Axum: .route("/path", METHOD(handler_ident))
;; Captures the handler identifier passed as argument to a method call (get/post/put/delete/patch)
;; that is itself the second argument to .route(...).
(call_expression
  function: (field_expression
    field: (field_identifier) @_route (#eq? @_route "route"))
  arguments: (arguments
    [(string_literal) @axum.route.path (MISSING) @axum.route.path]
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
      arguments: (token_tree
        [(string_literal) @actix.route.path (MISSING) @actix.route.path])?
      (#match? @actix.route.method "^(get|post|put|delete|patch|head)$")))
  .
  (function_item
    name: (identifier) @actix.route.handler))

;; ---- Redis Rust pub/sub (T5-31) ----
;; Covers the `redis` crate (sync and redis::aio async variants).
;; Import gate (`redis`) is enforced by REDIS_RUST.import_gate — these queries
;; fire on syntax alone; the extractor filters by import at runtime.
;;
;; `redis.direction` captures the method name so `classify_redis_direction`
;; can distinguish Subscribe from Publish.
;;
;; Topic literal: the first positional `string_literal` arg.
;; Variable channel args produce no `redis.topic` capture → no RawEventTopic.
;;
;; Anchored to `function_item` to co-capture the enclosing function name.
;; Async `.await` and `?` operator are transparent — the inner call_expression
;; shape is identical.

;; con.publish("channel", "msg") inside a function_item (bare expression) — Publish.
(function_item
  name: (identifier) @redis.fn
  body: (block
    (expression_statement
      (call_expression
        function: (field_expression
          field: (field_identifier) @redis.direction (#eq? @redis.direction "publish"))
        arguments: (arguments
          . (string_literal) @redis.topic)))))

;; let _: () = con.publish("channel", "msg")?; — Publish (try_expression form).
(function_item
  name: (identifier) @redis.fn
  body: (block
    (let_declaration
      (try_expression
        (call_expression
          function: (field_expression
            field: (field_identifier) @redis.direction (#eq? @redis.direction "publish"))
          arguments: (arguments
            . (string_literal) @redis.topic))))))

;; pubsub.subscribe("channel")?; — Subscribe (try_expression form).
(function_item
  name: (identifier) @redis.fn
  body: (block
    (expression_statement
      (try_expression
        (call_expression
          function: (field_expression
            field: (field_identifier) @redis.direction (#eq? @redis.direction "subscribe"))
          arguments: (arguments
            . (string_literal) @redis.topic))))))

;; pubsub.subscribe("channel"); — Subscribe (bare form, no ?).
(function_item
  name: (identifier) @redis.fn
  body: (block
    (expression_statement
      (call_expression
        function: (field_expression
          field: (field_identifier) @redis.direction (#eq? @redis.direction "subscribe"))
        arguments: (arguments
          . (string_literal) @redis.topic)))))

;; pubsub.psubscribe("pattern.*")?; — Subscribe (try_expression glob pattern).
(function_item
  name: (identifier) @redis.fn
  body: (block
    (expression_statement
      (try_expression
        (call_expression
          function: (field_expression
            field: (field_identifier) @redis.direction (#eq? @redis.direction "psubscribe"))
          arguments: (arguments
            . (string_literal) @redis.topic))))))

;; pubsub.psubscribe("pattern.*"); — Subscribe (bare glob pattern).
(function_item
  name: (identifier) @redis.fn
  body: (block
    (expression_statement
      (call_expression
        function: (field_expression
          field: (field_identifier) @redis.direction (#eq? @redis.direction "psubscribe"))
        arguments: (arguments
          . (string_literal) @redis.topic)))))

;; con.publish("channel", msg).await?; — Publish (async, try_expression wraps await_expression).
(function_item
  name: (identifier) @redis.fn
  body: (block
    (let_declaration
      (try_expression
        (await_expression
          (call_expression
            function: (field_expression
              field: (field_identifier) @redis.direction (#eq? @redis.direction "publish"))
            arguments: (arguments
              . (string_literal) @redis.topic)))))))

;; pubsub.subscribe("channel").await?; — Subscribe (async try form).
(function_item
  name: (identifier) @redis.fn
  body: (block
    (expression_statement
      (try_expression
        (await_expression
          (call_expression
            function: (field_expression
              field: (field_identifier) @redis.direction (#eq? @redis.direction "subscribe"))
            arguments: (arguments
              . (string_literal) @redis.topic)))))))

;; pubsub.psubscribe("pattern.*").await?; — Subscribe (async try psubscribe form).
(function_item
  name: (identifier) @redis.fn
  body: (block
    (expression_statement
      (try_expression
        (await_expression
          (call_expression
            function: (field_expression
              field: (field_identifier) @redis.direction (#eq? @redis.direction "psubscribe"))
            arguments: (arguments
              . (string_literal) @redis.topic)))))))
