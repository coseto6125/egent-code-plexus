;; Framework-aware queries for Rust (Tier 1: Axum/Actix routes + Redis pub/sub + RabbitMQ).

;; ---- Attribute-decorated functions / methods (FU-2026-05-23-009) ----
;; Captures `attribute_item` nodes that appear immediately before a function_item
;; or method in an impl block. Used to populate RawNode.decorators for
;; annotation-style proc-macros such as #[transaction], #[route], etc.
;;
;; Tree-sitter-rust grammar: attribute_item and function_item are *siblings*
;; under a common parent (source_file / declaration_list). The adjacent-sibling
;; anchor (.) ensures we only capture attributes directly preceding the function,
;; not unrelated attrs elsewhere in the same block.
;;
;; These patterns produce @decorator + @function_item.name + @function or
;; @decorator + @function_item.name + @method captures. The parser dedup merges
;; decorator lists onto the node already emitted by the base queries.scm patterns.

;; Top-level attributed functions (source_file parent)
(source_file
  (attribute_item) @decorator
  .
  (function_item
    (visibility_modifier)? @export
    name: (identifier) @function_item.name
    return_type: (_)? @type) @function)

;; Attributed functions inside mod bodies
(mod_item
  body: (declaration_list
    (attribute_item) @decorator
    .
    (function_item
      (visibility_modifier)? @export
      name: (identifier) @function_item.name
      return_type: (_)? @type) @function))

;; Attributed methods in trait-impl blocks
(impl_item
  trait: [
    (type_identifier)
    (generic_type)
  ] @heritage
  body: (declaration_list
    (attribute_item) @decorator
    .
    (function_item
      (visibility_modifier)? @export
      name: (identifier) @function_item.name
      return_type: (_)? @type) @method))

;; Attributed methods in inherent-impl blocks
(impl_item
  body: (declaration_list
    (attribute_item) @decorator
    .
    (function_item
      (visibility_modifier)? @export
      name: (identifier) @function_item.name
      return_type: (_)? @type) @method))

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

;; ---- AWS SQS Rust SDK (T5-19) ----
;; Covers the fluent-builder pattern used by `aws-sdk-sqs`:
;;   client.send_message().queue_url("https://…").send().await
;;   client.receive_message().queue_url("https://…").send().await
;;
;; Import gate (aws_sdk_sqs) enforced at runtime by SQS_RUST.
;;
;; Rust AST: the fluent chain evaluates left-to-right, so the AST node for
;; `send_message().queue_url("…")` is:
;;   call_expression                     ← the .queue_url("…") call
;;     field_expression
;;       value: call_expression          ← the .send_message() call
;;         field_expression
;;           field: "send_message"
;;       field: "queue_url"
;;     arguments: (arguments (string_literal))
;;
;; Outer wrappers (.message_body().send().await.unwrap()) add call_expression
;; layers on top — ignored; we only care about the queue_url call itself.
;;
;; No function anchor: tree-sitter Rust named-field `body: (block (...))` does
;; not provide descendant matching, and the call is buried too deep in the chain
;; for a fixed-depth body-anchored pattern to be reliable.  `enclosing_fn` is
;; left empty (pool.add("")) — acceptable; the real value is the topic + direction.
;;
;; When `.queue_url` receives a variable identifier (not a string literal), the
;; `(string_literal)` pattern does not match → no fabrication.

;; Publish — send_message / send_message_batch immediately before .queue_url("literal").
(call_expression
  function: (field_expression
    value: (call_expression
      function: (field_expression
        field: (field_identifier) @sqs.direction))
    field: (field_identifier) @_qurl)
  arguments: (arguments
    (string_literal) @sqs.topic)
  (#match? @sqs.direction "^(send_message|send_message_batch)$")
  (#eq? @_qurl "queue_url"))

;; Subscribe — receive_message immediately before .queue_url("literal").
(call_expression
  function: (field_expression
    value: (call_expression
      function: (field_expression
        field: (field_identifier) @sqs.direction))
    field: (field_identifier) @_qurl)
  arguments: (arguments
    (string_literal) @sqs.topic)
  (#eq? @sqs.direction "receive_message")
  (#eq? @_qurl "queue_url"))

;; ---- BlindSpot patterns (FU-001 P4) ----
;; transmute::<..., fn(...)>(ptr) — bit-cast to function pointer. Match
;; generic_function calls whose path ends in `transmute` and whose turbofish
;; contains a function_type. Non-fn transmutes (numeric reinterpret) are
;; ignored. Indirect dispatch through `dyn Trait` / Fn callbacks is handled
;; separately by `indirect_dispatch.rs` (CallMeta path) — do NOT duplicate here.
((call_expression
   function: (generic_function
     function: (_) @_fn
     type_arguments: (type_arguments
       (function_type)))) @blind.transmute_fn
  (#match? @_fn "transmute$"))

;; libloading::Library::get(...) — dynamic symbol load from a dlopen'd
;; library. Fully-qualified 3-segment form only; the 2-segment `Library::get`
;; (post `use libloading::Library;`) and method form `lib.get::<...>(...)`
;; are deferred follow-ups (need import-presence gate / type inference).
((call_expression
   function: (scoped_identifier
     path: (scoped_identifier
       path: (identifier) @_p1 (#eq? @_p1 "libloading")
       name: (identifier) @_p2 (#eq? @_p2 "Library"))
     name: (identifier) @_p3 (#eq? @_p3 "get"))) @blind.libloading_get)
