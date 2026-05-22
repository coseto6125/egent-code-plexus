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

;; ---- Kafka JavaScript (T5-4) ----
;; Covers kafkajs (`producer.send({ topic: '...', messages: [...] })`) and
;; node-rdkafka (`producer.produce('<topic>', partition, payload, ...)`).
;; Import gate (`kafkajs`, `node-rdkafka`) is enforced by KAFKA_JS.import_gate —
;; these queries fire on syntax alone; the extractor filters by import presence.
;;
;; Anchored to `function_declaration` / `method_definition` to co-capture the
;; enclosing function identifier alongside the topic literal in a single match.
;; Module-level Kafka calls are omitted — they represent <1% of production usage
;; and would produce a topic with empty enclosing_fn, offering no LLM
;; disambiguation value (same rationale as KAFKA_PYTHON).
;;
;; Two forms per anchor: sync and async/await.  The `await_expression` node wraps
;; the `call_expression` inside `expression_statement` for async calls, so it
;; requires a dedicated pattern separate from the plain sync form.  Sharing a
;; wildcard `_` for both would require an extra depth level that mismatches on
;; the sync case.

;; ── kafkajs: function_declaration ──

;; Sync: `producer.send({ topic: '<literal>', messages: [...] })`
(function_declaration
  name: (identifier) @kafka.producer_fn
  body: (statement_block
    (expression_statement
      (call_expression
        function: (member_expression
          property: (property_identifier) @_send (#eq? @_send "send"))
        arguments: (arguments
          (object
            (pair
              key: (property_identifier) @_tk (#eq? @_tk "topic")
              value: (string) @kafka.topic)))))))

;; Async: `await producer.send({ topic: '<literal>', messages: [...] })`
(function_declaration
  name: (identifier) @kafka.producer_fn
  body: (statement_block
    (expression_statement
      (await_expression
        (call_expression
          function: (member_expression
            property: (property_identifier) @_asend (#eq? @_asend "send"))
          arguments: (arguments
            (object
              (pair
                key: (property_identifier) @_atk (#eq? @_atk "topic")
                value: (string) @kafka.topic))))))))

;; ── kafkajs: method_definition ──

;; Sync: `producer.send({ topic: '<literal>', messages: [...] })`
(method_definition
  name: (property_identifier) @kafka.producer_fn
  body: (statement_block
    (expression_statement
      (call_expression
        function: (member_expression
          property: (property_identifier) @_msend (#eq? @_msend "send"))
        arguments: (arguments
          (object
            (pair
              key: (property_identifier) @_mtk (#eq? @_mtk "topic")
              value: (string) @kafka.topic)))))))

;; Async: `await producer.send({ topic: '<literal>', messages: [...] })`
(method_definition
  name: (property_identifier) @kafka.producer_fn
  body: (statement_block
    (expression_statement
      (await_expression
        (call_expression
          function: (member_expression
            property: (property_identifier) @_masend (#eq? @_masend "send"))
          arguments: (arguments
            (object
              (pair
                key: (property_identifier) @_matk (#eq? @_matk "topic")
                value: (string) @kafka.topic))))))))

;; ── node-rdkafka: function_declaration ──

;; Sync: `producer.produce('<topic>', partition, payload, ...)`
(function_declaration
  name: (identifier) @kafka.producer_fn
  body: (statement_block
    (expression_statement
      (call_expression
        function: (member_expression
          property: (property_identifier) @_produce (#eq? @_produce "produce"))
        arguments: (arguments
          . (string) @kafka.topic)))))

;; ── node-rdkafka: method_definition ──

;; Sync: `producer.produce('<topic>', ...)`
(method_definition
  name: (property_identifier) @kafka.producer_fn
  body: (statement_block
    (expression_statement
      (call_expression
        function: (member_expression
          property: (property_identifier) @_mproduce (#eq? @_mproduce "produce"))
        arguments: (arguments
          . (string) @kafka.topic)))))

;; ---- RabbitMQ JavaScript (T5-10) ----
;; Covers amqplib and amqp-connection-manager.
;; Import gate (`amqplib`, `amqp-connection-manager`) is enforced by
;; RABBITMQ_JS.import_gate — these queries fire on syntax alone.
;;
;; `amqp.direction` captures the method name so `classify_amqp_direction`
;; can distinguish Subscribe (consume/assertQueue) from Publish (publish/sendToQueue).
;;
;; Topic literal:
;;   publish(exchange, routingKey, content) → routingKey = 2nd positional string.
;;   consume(queue, handler)               → queue      = 1st positional string.
;;   assertQueue(queue, opts)              → queue      = 1st positional string.
;;   sendToQueue(queue, content)           → queue      = 1st positional string.
;;
;; Variable args → no capture. Anchored to function_declaration and
;; method_definition; sync + await forms.

;; publish(exchange, routingKey, ...) — sync, function_declaration.
(function_declaration
  name: (identifier) @amqp.fn
  body: (statement_block
    (_
      (call_expression
        function: (member_expression
          property: (property_identifier) @amqp.direction
          (#eq? @amqp.direction "publish"))
        arguments: (arguments
          . (_)
          . (string) @amqp.topic)))))

;; publish — await, function_declaration.
(function_declaration
  name: (identifier) @amqp.fn
  body: (statement_block
    (_
      (await_expression
        (call_expression
          function: (member_expression
            property: (property_identifier) @amqp.direction
            (#eq? @amqp.direction "publish"))
          arguments: (arguments
            . (_)
            . (string) @amqp.topic))))))

;; consume / assertQueue / sendToQueue — sync, function_declaration.
(function_declaration
  name: (identifier) @amqp.fn
  body: (statement_block
    (_
      (call_expression
        function: (member_expression
          property: (property_identifier) @amqp.direction
          (#match? @amqp.direction "^(consume|assertQueue|sendToQueue)$"))
        arguments: (arguments
          . (string) @amqp.topic)))))

;; consume / assertQueue / sendToQueue — await, function_declaration.
(function_declaration
  name: (identifier) @amqp.fn
  body: (statement_block
    (_
      (await_expression
        (call_expression
          function: (member_expression
            property: (property_identifier) @amqp.direction
            (#match? @amqp.direction "^(consume|assertQueue|sendToQueue)$"))
          arguments: (arguments
            . (string) @amqp.topic))))))

;; publish — sync, method_definition.
(method_definition
  name: (property_identifier) @amqp.fn
  body: (statement_block
    (_
      (call_expression
        function: (member_expression
          property: (property_identifier) @amqp.direction
          (#eq? @amqp.direction "publish"))
        arguments: (arguments
          . (_)
          . (string) @amqp.topic)))))

;; publish — await, method_definition.
(method_definition
  name: (property_identifier) @amqp.fn
  body: (statement_block
    (_
      (await_expression
        (call_expression
          function: (member_expression
            property: (property_identifier) @amqp.direction
            (#eq? @amqp.direction "publish"))
          arguments: (arguments
            . (_)
            . (string) @amqp.topic))))))

;; consume / assertQueue / sendToQueue — sync, method_definition.
(method_definition
  name: (property_identifier) @amqp.fn
  body: (statement_block
    (_
      (call_expression
        function: (member_expression
          property: (property_identifier) @amqp.direction
          (#match? @amqp.direction "^(consume|assertQueue|sendToQueue)$"))
        arguments: (arguments
          . (string) @amqp.topic)))))

;; consume / assertQueue / sendToQueue — await, method_definition.
(method_definition
  name: (property_identifier) @amqp.fn
  body: (statement_block
    (_
      (await_expression
        (call_expression
          function: (member_expression
            property: (property_identifier) @amqp.direction
            (#match? @amqp.direction "^(consume|assertQueue|sendToQueue)$"))
          arguments: (arguments
            . (string) @amqp.topic))))))

;; ---- AWS SQS JavaScript (T5-16) ----
;; Covers @aws-sdk/client-sqs v3 command pattern:
;;   await client.send(new SendMessageCommand({ QueueUrl: "https://...", MessageBody: "..." }))
;;   await client.send(new ReceiveMessageCommand({ QueueUrl: "https://...", ... }))
;;   await client.send(new SendMessageBatchCommand({ QueueUrl: "https://...", ... }))
;;   await client.send(new DeleteMessageCommand({ QueueUrl: "https://...", ... }))
;;
;; Import gate (`@aws-sdk/client-sqs`) enforced by SQS_JS.import_gate.
;;
;; `sqs.direction` captures the Command constructor identifier so
;; `classify_sqs_direction` can map to Publish vs Subscribe.
;; `sqs.topic` captures the `QueueUrl` property string value.
;; `sqs.fn` captures the enclosing function/method name.
;;
;; Non-literal QueueUrl (variable/expression) produces no string node
;; → no match → no fabrication.
;;
;; Anchored to `function_declaration` and `method_definition` to co-capture
;; the enclosing function name. Both sync and await forms handled.

;; SQS: function_declaration (await form — the common case in AWS SDK v3).
(function_declaration
  name: (identifier) @sqs.fn
  body: (statement_block
    (expression_statement
      (await_expression
        (call_expression
          function: (member_expression
            property: (property_identifier) @_send (#eq? @_send "send"))
          arguments: (arguments
            (new_expression
              constructor: (identifier) @sqs.direction
              (#match? @sqs.direction "^(SendMessageCommand|SendMessageBatchCommand|ReceiveMessageCommand|DeleteMessageCommand)$")
              arguments: (arguments
                (object
                  (pair
                    key: (property_identifier) @_qk (#eq? @_qk "QueueUrl")
                    value: (string) @sqs.topic))))))))))

;; SQS: method_definition (await form).
(method_definition
  name: (property_identifier) @sqs.fn
  body: (statement_block
    (expression_statement
      (await_expression
        (call_expression
          function: (member_expression
            property: (property_identifier) @_send (#eq? @_send "send"))
          arguments: (arguments
            (new_expression
              constructor: (identifier) @sqs.direction
              (#match? @sqs.direction "^(SendMessageCommand|SendMessageBatchCommand|ReceiveMessageCommand|DeleteMessageCommand)$")
              arguments: (arguments
                (object
                  (pair
                    key: (property_identifier) @_qk (#eq? @_qk "QueueUrl")
                    value: (string) @sqs.topic))))))))))

;; SQS: function_declaration (sync / Promise.then form).
(function_declaration
  name: (identifier) @sqs.fn
  body: (statement_block
    (expression_statement
      (call_expression
        function: (member_expression
          property: (property_identifier) @_send (#eq? @_send "send"))
        arguments: (arguments
          (new_expression
            constructor: (identifier) @sqs.direction
            (#match? @sqs.direction "^(SendMessageCommand|SendMessageBatchCommand|ReceiveMessageCommand|DeleteMessageCommand)$")
            arguments: (arguments
              (object
                (pair
                  key: (property_identifier) @_qk (#eq? @_qk "QueueUrl")
                  value: (string) @sqs.topic)))))))))

;; SQS: method_definition (sync form).
(method_definition
  name: (property_identifier) @sqs.fn
  body: (statement_block
    (expression_statement
      (call_expression
        function: (member_expression
          property: (property_identifier) @_send (#eq? @_send "send"))
        arguments: (arguments
          (new_expression
            constructor: (identifier) @sqs.direction
            (#match? @sqs.direction "^(SendMessageCommand|SendMessageBatchCommand|ReceiveMessageCommand|DeleteMessageCommand)$")
            arguments: (arguments
              (object
                (pair
                  key: (property_identifier) @_qk (#eq? @_qk "QueueUrl")
                  value: (string) @sqs.topic)))))))))
