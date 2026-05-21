;; Framework-aware queries for JavaScript.

;; ---- Redis JavaScript (T5-28) ----
;; Covers node-redis v4 (`client.publish/subscribe/pSubscribe`) and
;; ioredis (`redis.publish/subscribe/psubscribe`).
;; Import gate (`redis`, `ioredis`) is enforced by REDIS_JS.import_gate.
;;
;; Direction capture (`redis.direction`) holds the method name so
;; `classify_redis_direction` can map subscribe/pSubscribe/psubscribe →
;; PubSub::Subscribe and everything else → PubSub::Publish.
;;
;; Sync and await forms require SEPARATE patterns — `await_expression` wraps
;; `call_expression` inside `expression_statement` for async calls, so a
;; wildcard `_` intermediate node would add a depth level that mismatches
;; the sync case.
;;
;; Predicates: `#match?` matches any of publish|subscribe|pSubscribe|psubscribe.
;; Tested in: tests/javascript_redis_events.rs

;; ── Redis: function_declaration (sync) ──

(function_declaration
  name: (identifier) @redis.fn
  body: (statement_block
    (expression_statement
      (call_expression
        function: (member_expression
          property: (property_identifier) @redis.direction
            (#match? @redis.direction "^(publish|subscribe|pSubscribe|psubscribe)$"))
        arguments: (arguments
          . (string) @redis.topic)))))

;; ── Redis: function_declaration (async/await) ──

(function_declaration
  name: (identifier) @redis.fn
  body: (statement_block
    (expression_statement
      (await_expression
        (call_expression
          function: (member_expression
            property: (property_identifier) @redis.direction
              (#match? @redis.direction "^(publish|subscribe|pSubscribe|psubscribe)$"))
          arguments: (arguments
            . (string) @redis.topic))))))

;; ── Redis: method_definition (sync) ──

(method_definition
  name: (property_identifier) @redis.fn
  body: (statement_block
    (expression_statement
      (call_expression
        function: (member_expression
          property: (property_identifier) @redis.direction
            (#match? @redis.direction "^(publish|subscribe|pSubscribe|psubscribe)$"))
        arguments: (arguments
          . (string) @redis.topic)))))

;; ── Redis: method_definition (async/await) ──

(method_definition
  name: (property_identifier) @redis.fn
  body: (statement_block
    (expression_statement
      (await_expression
        (call_expression
          function: (member_expression
            property: (property_identifier) @redis.direction
              (#match? @redis.direction "^(publish|subscribe|pSubscribe|psubscribe)$"))
          arguments: (arguments
            . (string) @redis.topic))))))
