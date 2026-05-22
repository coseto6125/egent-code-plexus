;; Framework-aware queries for TypeScript (Tier 1: Express subset).

;; Express: app.{get,post,put,delete,patch,use}(<path_str>, <handler_ident>)
;; Captures the handler identifier passed as second argument.
(call_expression
  function: (member_expression
    object: (identifier)
    property: (property_identifier) @express.route.method
    (#match? @express.route.method "^(get|post|put|delete|patch|use)$"))
  arguments: (arguments
    [(string) @express.route.path (MISSING) @express.route.path]
    (identifier) @express.route.handler))

;; NestJS: @Controller-decorated class with @Get/@Post/@Put/@Delete/@Patch
;; method-level decorators. Two forms — class is exported via `export class`
;; (decorator moves to export_statement) or declared directly (decorator stays
;; on class_declaration).

;; Form 1: non-exported @Controller class.
(class_declaration
  (decorator
    (call_expression
      function: (identifier) @nestjs.controller.kw
      (#eq? @nestjs.controller.kw "Controller")))
  name: (type_identifier) @nestjs.controller.class
  body: (class_body
    (decorator
      (call_expression
        function: (identifier) @nestjs.method.verb
        (#match? @nestjs.method.verb "^(Get|Post|Put|Delete|Patch)$")))
    .
    (method_definition
      name: (property_identifier) @nestjs.method.name)))

;; Form 2: exported @Controller class — decorator sits on export_statement.
(export_statement
  (decorator
    (call_expression
      function: (identifier) @nestjs.controller.kw
      (#eq? @nestjs.controller.kw "Controller")))
  declaration: (class_declaration
    name: (type_identifier) @nestjs.controller.class
    body: (class_body
      (decorator
        (call_expression
          function: (identifier) @nestjs.method.verb
          (#match? @nestjs.method.verb "^(Get|Post|Put|Delete|Patch)$")))
      .
      (method_definition
        name: (property_identifier) @nestjs.method.name))))

;; ---- TypeScript interface SchemaField (T4-4) ----
;; Captures typed property signatures on `interface X { ... }` bodies.
;; Each property_signature with a predefined_type annotation becomes one
;; RawSchemaField via the T4-1 dispatcher (TS_INTERFACE_CONFIG).
;; `predefined_type` covers: string, number, boolean, any, void, never, object,
;; symbol, bigint, undefined, null.  Union (`string | null`) and array
;; (`string[]`) are `union_type` / `array_type` — they don't match this
;; pattern and fall through to SchemaType::Other via classify_ts_type("").
;; No import gate needed: `interface` is a TS language built-in.
(interface_declaration
  name: (type_identifier) @ts.owner
  body: (interface_body
    (property_signature
      name: (property_identifier) @ts.field
      type: (type_annotation
        (predefined_type) @ts.type))))

;; ---- RabbitMQ TypeScript (T5-9) ----
;; Covers amqplib and amqp-connection-manager.
;; Import gate (`amqplib`, `amqp-connection-manager`) is enforced by
;; RABBITMQ_TS.import_gate — these queries fire on syntax alone.
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
;; Variable args → no capture (`. (string)` anchors require a literal).
;; Anchored to function_declaration and method_definition; sync + await forms.

;; publish(exchange, routingKey, ...) — sync, function_declaration.
;; The 2nd positional arg (routingKey) is captured: `. _ . (string)`.
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

;; consume(queue, handler) / assertQueue(queue) / sendToQueue(queue, ...) — sync, function_declaration.
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

;; NestJS / generic decorator-route: `@Get('users')` / `@Post('users/:id')` /
;; `@Put('audio/transcode')`. Captures the decorator verb AND the bare path
;; argument. Independent of `@Controller` context — gated in parser.rs by
;; `has_nestjs` (only imports of `@nestjs/*` flip the flag), so user-defined
;; `@Get(...)` decorators in non-NestJS code don't surface false routes.
;;
;; Verb list mirrors NestJS's HTTP routing decorators (omits `@All` which
;; tree-sitter captures via its own grammar path and routes to the generic
;; `app.METHOD()` matcher above).
(decorator
  (call_expression
    function: (identifier) @nestjs.decorator.verb
    (#match? @nestjs.decorator.verb "^(Get|Post|Put|Delete|Patch|Options|Head|All)$")
    arguments: (arguments
      [(string (string_fragment) @nestjs.decorator.path) (MISSING) @nestjs.decorator.path])))

;; ---- Redis TypeScript (T5-27) ----
;; Covers node-redis v4 (`client.publish/subscribe/pSubscribe(...)`) and
;; ioredis (`redis.publish/subscribe/psubscribe(...)`).
;; Import gate (`redis`, `ioredis`) is enforced by REDIS_TS.import_gate —
;; these queries fire on syntax alone; the extractor filters by import at runtime.
;;
;; `redis.direction` captures the method name so `classify_redis_direction`
;; can distinguish Subscribe from Publish.  node-redis v4 spells it `pSubscribe`
;; (camelCase); ioredis spells it `psubscribe` (lowercase) — two separate
;; `#eq?` predicates cover both forms without regex.
;;
;; Anchored to `function_declaration` and `method_definition`; sync and
;; await forms handled separately (mirrors T5-3 Kafka TS approach).
;;
;; Channel must be the first positional string literal arg (`. (string)`);
;; variable channels emit nothing — no fabrication.

;; Redis: `client.publish/subscribe/pSubscribe/psubscribe('<channel>', ...)` inside function_declaration (sync).
(function_declaration
  name: (identifier) @redis.fn
  body: (statement_block
    (_
      (call_expression
        function: (member_expression
          property: (property_identifier) @redis.direction
          (#match? @redis.direction "^(publish|subscribe|pSubscribe|psubscribe)$"))
        arguments: (arguments
          . (string) @redis.topic)))))

;; Redis: `await client.publish/subscribe/pSubscribe/psubscribe('<channel>', ...)` inside async function_declaration.
(function_declaration
  name: (identifier) @redis.fn
  body: (statement_block
    (_
      (await_expression
        (call_expression
          function: (member_expression
            property: (property_identifier) @redis.direction
            (#match? @redis.direction "^(publish|subscribe|pSubscribe|psubscribe)$"))
          arguments: (arguments
            . (string) @redis.topic))))))

;; Redis: `client.publish/subscribe/pSubscribe/psubscribe('<channel>', ...)` inside method_definition (sync).
(method_definition
  name: (property_identifier) @redis.fn
  body: (statement_block
    (_
      (call_expression
        function: (member_expression
          property: (property_identifier) @redis.direction
          (#match? @redis.direction "^(publish|subscribe|pSubscribe|psubscribe)$"))
        arguments: (arguments
          . (string) @redis.topic)))))

;; Redis: `await client.publish/subscribe/pSubscribe/psubscribe('<channel>', ...)` inside async method_definition.
(method_definition
  name: (property_identifier) @redis.fn
  body: (statement_block
    (_
      (await_expression
        (call_expression
          function: (member_expression
            property: (property_identifier) @redis.direction
            (#match? @redis.direction "^(publish|subscribe|pSubscribe|psubscribe)$"))
          arguments: (arguments
            . (string) @redis.topic))))))

;; ---- Kafka TypeScript (T5-3) ----
;; Covers kafkajs (`producer.send({ topic: '...', messages: [...] })`) and
;; node-rdkafka (`producer.produce('topic-name', ...)`).
;; Import gate (`kafkajs`, `node-rdkafka`) is enforced by KAFKA_TS.import_gate —
;; these queries fire on syntax alone; the extractor filters by import at runtime.
;;
;; Anchored to `function_declaration` and `method_definition` to co-capture the
;; enclosing function/method name alongside the topic literal in a single match.
;; Module-level Kafka calls are omitted — <1% real-world signal with no LLM
;; disambiguation value (empty enclosing_fn).
;;
;; `sendBatch` (topicMessages: [{ topic: '...', messages: [...] }]) is a nested
;; array form — the triple-nesting makes a unique predicate-free pattern;
;; deferred to T5-3-followup once tree-sitter quantifier support is confirmed.

;; kafkajs: `producer.send({ topic: '<literal>', ... })` inside a function_declaration (sync).
(function_declaration
  name: (identifier) @kafka.producer_fn
  body: (statement_block
    (_
      (call_expression
        function: (member_expression
          property: (property_identifier) @_send
          (#eq? @_send "send"))
        arguments: (arguments
          (object
            (pair
              key: (property_identifier) @_topic_key
              (#eq? @_topic_key "topic")
              value: (string) @kafka.topic)))))))

;; kafkajs: `await producer.send({ topic: '<literal>', ... })` inside an async function_declaration.
;; The await_expression wraps the call — separate pattern from the sync form.
(function_declaration
  name: (identifier) @kafka.producer_fn
  body: (statement_block
    (_
      (await_expression
        (call_expression
          function: (member_expression
            property: (property_identifier) @_asend
            (#eq? @_asend "send"))
          arguments: (arguments
            (object
              (pair
                key: (property_identifier) @_atopic_key
                (#eq? @_atopic_key "topic")
                value: (string) @kafka.topic))))))))

;; kafkajs: `producer.send({ topic: '<literal>', ... })` inside a method_definition (sync).
(method_definition
  name: (property_identifier) @kafka.producer_fn
  body: (statement_block
    (_
      (call_expression
        function: (member_expression
          property: (property_identifier) @_msend
          (#eq? @_msend "send"))
        arguments: (arguments
          (object
            (pair
              key: (property_identifier) @_mtopic_key
              (#eq? @_mtopic_key "topic")
              value: (string) @kafka.topic)))))))

;; kafkajs: `await producer.send({ topic: '<literal>', ... })` inside an async method_definition.
;; The await_expression wraps the call — needs a separate pattern from the sync form.
(method_definition
  name: (property_identifier) @kafka.producer_fn
  body: (statement_block
    (_
      (await_expression
        (call_expression
          function: (member_expression
            property: (property_identifier) @_masend
            (#eq? @_masend "send"))
          arguments: (arguments
            (object
              (pair
                key: (property_identifier) @_matopic_key
                (#eq? @_matopic_key "topic")
                value: (string) @kafka.topic))))))))

;; node-rdkafka: `producer.produce('<topic>', partition, payload, ...)` inside a function_declaration.
;; First positional arg must be a string literal.
(function_declaration
  name: (identifier) @kafka.producer_fn
  body: (statement_block
    (_
      (call_expression
        function: (member_expression
          property: (property_identifier) @_produce
          (#eq? @_produce "produce"))
        arguments: (arguments
          . (string) @kafka.topic)))))

;; node-rdkafka: `producer.produce('<topic>', ...)` inside a method_definition.
(method_definition
  name: (property_identifier) @kafka.producer_fn
  body: (statement_block
    (_
      (call_expression
        function: (member_expression
          property: (property_identifier) @_mproduce
          (#eq? @_mproduce "produce"))
        arguments: (arguments
          . (string) @kafka.topic)))))

;; ---- AWS SQS TypeScript (T5-15) ----
;; Covers @aws-sdk/client-sqs v3 command pattern:
;;   await client.send(new SendMessageCommand({ QueueUrl: "https://...", MessageBody: "..." }))
;;   await client.send(new ReceiveMessageCommand({ QueueUrl: "https://...", ... }))
;;   await client.send(new SendMessageBatchCommand({ QueueUrl: "https://...", ... }))
;;   await client.send(new DeleteMessageCommand({ QueueUrl: "https://...", ... }))
;;
;; Import gate (`@aws-sdk/client-sqs`) enforced by SQS_TS.import_gate.
;;
;; `sqs.direction` captures the Command constructor identifier so
;; `classify_sqs_direction` can map to Publish vs Subscribe.
;; `sqs.topic` captures the `QueueUrl` property string_fragment.
;; `sqs.fn` captures the enclosing function/method name.
;;
;; Non-literal QueueUrl (variable/expression) produces no `string_fragment`
;; → no match → no fabrication.
;;
;; Anchored to `function_declaration` and `method_definition` to co-capture
;; the enclosing function name. Both sync and await forms handled.

;; SQS: function_declaration (await form — the common case in AWS SDK v3).
(function_declaration
  name: (identifier) @sqs.fn
  body: (statement_block
    (_
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
                    value: (string
                      (string_fragment) @sqs.topic)))))))))))

;; SQS: method_definition (await form).
(method_definition
  name: (property_identifier) @sqs.fn
  body: (statement_block
    (_
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
                    value: (string
                      (string_fragment) @sqs.topic)))))))))))

;; SQS: function_declaration (sync form — for non-async wrappers or Promise.then chains).
(function_declaration
  name: (identifier) @sqs.fn
  body: (statement_block
    (_
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
                  value: (string
                    (string_fragment) @sqs.topic))))))))))

;; SQS: method_definition (sync form).
(method_definition
  name: (property_identifier) @sqs.fn
  body: (statement_block
    (_
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
                  value: (string
                    (string_fragment) @sqs.topic))))))))))
