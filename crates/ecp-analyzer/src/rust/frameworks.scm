;; Framework-aware queries for Rust (Tier 1: Axum/Actix routes + Redis pub/sub + RabbitMQ).

;; ---- RabbitMQ Rust (T5-13) ----
;; Covers lapin (async) and amiquip (sync-friendly).
;; Import gate (`lapin`, `amiquip`) enforced by RABBITMQ_RUST.import_gate.
;;
;; `amqp.direction` captures the method name:
;;   channel.basic_publish(exchange, routing_key, ...) → routing_key = 2nd positional.
;;   channel.basic_consume(queue, ...) / channel.basic_get(queue, ...) → queue = 1st positional.
;;
;; Flat call_expression patterns (no function_item anchor) — lapin calls are
;; typically chained as `.basic_publish(...).await.unwrap()`, placing the
;; basic_publish call_expression inside an await_expression > field_expression
;; chain rather than as a direct block child. Import gate provides isolation.
;; Variable args → no capture (string_literal required).

;; basic_publish(exchange, routing_key, options, payload, properties)
;; Captures routing_key (2nd positional string_literal) as topic.
(call_expression
  function: (field_expression
    field: (field_identifier) @amqp.direction
    (#eq? @amqp.direction "basic_publish"))
  arguments: (arguments
    . (_)
    . (string_literal) @amqp.topic))

;; basic_consume(queue, consumer_tag, options, fields) / basic_get(queue, no_ack)
;; Captures queue (1st positional string_literal) as topic.
(call_expression
  function: (field_expression
    field: (field_identifier) @amqp.direction
    (#match? @amqp.direction "^(basic_consume|basic_get)$"))
  arguments: (arguments
    . (string_literal) @amqp.topic))

;; ---- Kafka Rust (T5-7) ----
;; Covers rdkafka: FutureRecord::to("topic") producer and
;; consumer.subscribe(&["topic"]) consumer.
;; Import gate (rdkafka) is enforced by KAFKA_RUST.import_gate — these
;; queries fire on syntax alone; the extractor filters by import at runtime.
;;
;; Anchored to `function_item` to co-capture the enclosing function name.
;; Variable topic args produce no capture (no fabrication).

;; rdkafka producer: producer.send(FutureRecord::to("topic"), ...)
;; Captures the string literal in FutureRecord::to("topic") inside a function_item.
(function_item
  name: (identifier) @kafka.rust.fn
  body: (block
    (_
      (call_expression
        function: (field_expression
          field: (field_identifier) @kafka.rust.direction
          (#eq? @kafka.rust.direction "send"))
        arguments: (arguments
          (call_expression
            function: (field_expression
              value: (call_expression
                function: (scoped_identifier) @_future_record_to
                (#match? @_future_record_to "FutureRecord::to")
                arguments: (arguments
                  (string_literal) @kafka.topic))))
          (_))))))

;; rdkafka producer: await form — producer.send(...).await inside function_item.
(function_item
  name: (identifier) @kafka.rust.fn
  body: (block
    (_
      (await_expression
        (call_expression
          function: (field_expression
            field: (field_identifier) @kafka.rust.direction
            (#eq? @kafka.rust.direction "send"))
          arguments: (arguments
            (call_expression
              function: (field_expression
                value: (call_expression
                  function: (scoped_identifier) @_afuture_record_to
                  (#match? @_afuture_record_to "FutureRecord::to")
                  arguments: (arguments
                    (string_literal) @kafka.topic))))
            (_)))))))

;; rdkafka consumer: consumer.subscribe(&["topic", ...]) inside function_item.
;; Captures the first string_literal in the slice literal.
;; NOTE: subscribe() is commonly chained: consumer.subscribe(&[...]).expect(...)
;; The subscribe call_expression is the `value` of the outer field_expression.
;; Match it via the field_expression parent's value field.
(function_item
  name: (identifier) @kafka.rust.fn
  body: (block
    (_
      (call_expression
        function: (field_expression
          value: (call_expression
            function: (field_expression
              field: (field_identifier) @kafka.rust.direction
              (#eq? @kafka.rust.direction "subscribe"))
            arguments: (arguments
              (reference_expression
                (array_expression
                  . (string_literal) @kafka.topic)))))))))

;; ---- Axum router subset ----

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
