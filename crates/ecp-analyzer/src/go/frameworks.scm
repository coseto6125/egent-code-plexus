;; Framework-aware queries for Go.

;; ---- Redis Go pub/sub (T5-30) ----
;; Covers go-redis (v8/v9) and gomodule/redigo under one config slice.
;; Import gate (github.com/redis/go-redis / github.com/go-redis/redis /
;; github.com/gomodule/redigo) is enforced by REDIS_GO.import_gate — these
;; queries fire on syntax alone; the extractor filters by import at runtime.
;;
;; `redis.direction` captures the method name so `classify_redis_direction`
;; can distinguish Subscribe from Publish.
;;
;; go-redis uses PascalCase: `Publish`, `Subscribe`, `PSubscribe`.
;; redigo uses lowercase via psc object: `subscribe` (via PubSubConn).
;;
;; Topic literal: the second positional interpreted_string_literal arg (go-redis
;; calls pass ctx as the first arg: Publish(ctx, "channel", msg)).
;; For redigo Subscribe/PSubscribe the channel is the first positional arg.
;;
;; Variable channel args produce no `redis.topic` capture → no RawEventTopic.
;;
;; Anchored to `function_declaration` to co-capture the enclosing function name.
;; The Go block has the shape: block → statement_list → expression_statement →
;; call_expression. We use `(block (statement_list (expression_statement ...)))`
;; to traverse the exact path without wildcard ambiguity.

;; go-redis: client.Publish(ctx, "channel", msg) inside a function — Publish.
;; Channel is the second positional arg (after ctx).
(function_declaration
  name: (identifier) @redis.fn
  body: (block
    (statement_list
      (expression_statement
        (call_expression
          function: (selector_expression
            field: (field_identifier) @redis.direction (#eq? @redis.direction "Publish"))
          arguments: (argument_list
            _
            (interpreted_string_literal) @redis.topic))))))

;; go-redis: client.Subscribe(ctx, "channel") inside a function — Subscribe.
;; Channel is the second positional arg (after ctx).
(function_declaration
  name: (identifier) @redis.fn
  body: (block
    (statement_list
      (expression_statement
        (call_expression
          function: (selector_expression
            field: (field_identifier) @redis.direction (#eq? @redis.direction "Subscribe"))
          arguments: (argument_list
            _
            (interpreted_string_literal) @redis.topic))))))

;; go-redis: client.PSubscribe(ctx, "pattern.*") inside a function — Subscribe (pattern).
;; Pattern is the second positional arg (after ctx).
(function_declaration
  name: (identifier) @redis.fn
  body: (block
    (statement_list
      (expression_statement
        (call_expression
          function: (selector_expression
            field: (field_identifier) @redis.direction (#eq? @redis.direction "PSubscribe"))
          arguments: (argument_list
            _
            (interpreted_string_literal) @redis.topic))))))


;; redigo: psc.Subscribe("channel") inside a function — Subscribe.
;; Channel is the first positional arg (no ctx).
(function_declaration
  name: (identifier) @redis.fn
  body: (block
    (statement_list
      (expression_statement
        (call_expression
          function: (selector_expression
            field: (field_identifier) @redis.direction (#eq? @redis.direction "subscribe"))
          arguments: (argument_list
            . (interpreted_string_literal) @redis.topic))))))

;; go-redis: short-var: pubsub := client.Subscribe(ctx, "channel") — Subscribe.
(function_declaration
  name: (identifier) @redis.fn
  body: (block
    (statement_list
      (short_var_declaration
        right: (expression_list
          (call_expression
            function: (selector_expression
              field: (field_identifier) @redis.direction (#eq? @redis.direction "Subscribe"))
            arguments: (argument_list
              _
              (interpreted_string_literal) @redis.topic)))))))

;; go-redis: short-var: pubsub := client.PSubscribe(ctx, "pattern.*") — Subscribe.
(function_declaration
  name: (identifier) @redis.fn
  body: (block
    (statement_list
      (short_var_declaration
        right: (expression_list
          (call_expression
            function: (selector_expression
              field: (field_identifier) @redis.direction (#eq? @redis.direction "PSubscribe"))
            arguments: (argument_list
              _
              (interpreted_string_literal) @redis.topic)))))))

;; ---- Kafka Go (T5-6) ----
;; Covers segmentio/kafka-go (WriteMessages) and Shopify/sarama (ProducerMessage).
;; Import gate (github.com/segmentio/kafka-go, github.com/Shopify/sarama,
;; github.com/confluentinc/confluent-kafka-go/kafka) is enforced by
;; KAFKA_GO.import_gate — these queries fire on syntax alone; the extractor
;; filters by import at runtime.
;;
;; Anchored to `function_declaration` or `method_declaration` to co-capture
;; the enclosing function name alongside the topic literal.
;; Variable topic args produce no capture (no fabrication).
;;
;; Go tree-sitter field names (tree-sitter-go 0.25):
;;   function_declaration: name, parameters, body
;;   method_declaration: receiver, name, parameters, body (name is field_identifier)
;;   block → statement_list → expression_statement (block CANNOT directly contain
;;   expression_statement — statement_list is always the intermediate node)
;;   call_expression: function, arguments
;;   selector_expression: operand, field
;;   composite_literal: type, body (literal_value)
;;   keyed_element: key (literal_element), value (literal_element)
;;   qualified_type: package (package_identifier), name (type_identifier)

;; segmentio/kafka-go: writer.WriteMessages(ctx, kafka.Message{Topic: "topic", ...})
;; Anchored on function_declaration; captures enclosing function name.
(function_declaration
  name: (identifier) @kafka.go.fn
  body: (block
    (statement_list
      (expression_statement
        (call_expression
          function: (selector_expression
            field: (field_identifier) @kafka.go.direction
            (#eq? @kafka.go.direction "WriteMessages"))
          arguments: (argument_list
            (_)
            (composite_literal
              body: (literal_value
                (keyed_element
                  key: (literal_element (identifier) @_topic_key
                    (#eq? @_topic_key "Topic"))
                  value: (literal_element
                    (interpreted_string_literal) @kafka.topic))))))))))


;; segmentio/kafka-go: WriteMessages inside a method_declaration.
(method_declaration
  name: (field_identifier) @kafka.go.fn
  body: (block
    (statement_list
      (expression_statement
        (call_expression
          function: (selector_expression
            field: (field_identifier) @kafka.go.direction
            (#eq? @kafka.go.direction "WriteMessages"))
          arguments: (argument_list
            (_)
            (composite_literal
              body: (literal_value
                (keyed_element
                  key: (literal_element (identifier) @_mtopic_key
                    (#eq? @_mtopic_key "Topic"))
                  value: (literal_element
                    (interpreted_string_literal) @kafka.topic))))))))))


;; Shopify/sarama: msg := &sarama.ProducerMessage{Topic: "topic", ...}
;; Captures Topic string literal directly from the struct literal.
;; The `&` unary operator wraps the composite_literal; the struct type's
;; `name` field (type_identifier) is matched against "ProducerMessage".
(function_declaration
  name: (identifier) @kafka.go.fn
  body: (block
    (statement_list
      (short_var_declaration
        (expression_list
          (unary_expression
            (composite_literal
              type: (qualified_type
                name: (type_identifier) @_sarama_type
                (#eq? @_sarama_type "ProducerMessage"))
              body: (literal_value
                (keyed_element
                  key: (literal_element (identifier) @_stopic_key
                    (#eq? @_stopic_key "Topic"))
                  value: (literal_element
                    (interpreted_string_literal) @kafka.topic))))))))))

;; ---- AWS SQS Go SDK v2 (T5-18) ----
;; Pattern: client.SendMessage(ctx, &sqs.SendMessageInput{QueueUrl: aws.String("url")})
;;          client.ReceiveMessage(ctx, &sqs.ReceiveMessageInput{QueueUrl: aws.String("url")})
;;
;; Anchored to function_declaration to capture the enclosing function name
;; alongside the topic literal in a single match.
;; Import gate (github.com/aws/aws-sdk-go-v2/service/sqs) enforced at runtime.
;;
;; Go AST structure (tree-sitter):
;;   block > statement_list > expression_statement > call_expression
;;     arguments > unary_expression > composite_literal
;;       body > literal_value > keyed_element
;;         key: literal_element > identifier("QueueUrl")
;;         value: literal_element > call_expression(aws.String)
;;           arguments > interpreted_string_literal("url")
;;
;; Dynamic QueueUrl (variable identifier, not string literal inside aws.String)
;; does not produce an interpreted_string_literal → no match → no fabrication.

;; Publish — SendMessage / SendMessageBatch with literal QueueUrl.
(function_declaration
  name: (identifier) @sqs.producer_fn
  body: (block
    (statement_list
      (expression_statement
        (call_expression
          function: (selector_expression
            field: (field_identifier) @sqs.direction)
          arguments: (argument_list
            (unary_expression
              operand: (composite_literal
                body: (literal_value
                  (keyed_element
                    key: (literal_element
                      (identifier) @_qk)
                    value: (literal_element
                      (call_expression
                        arguments: (argument_list
                          (interpreted_string_literal) @sqs.topic))))))))
          (#match? @sqs.direction "^(SendMessage|SendMessageBatch)$")
          (#eq? @_qk "QueueUrl"))))))

;; Subscribe — ReceiveMessage with literal QueueUrl.
(function_declaration
  name: (identifier) @sqs.producer_fn
  body: (block
    (statement_list
      (expression_statement
        (call_expression
          function: (selector_expression
            field: (field_identifier) @sqs.direction)
          arguments: (argument_list
            (unary_expression
              operand: (composite_literal
                body: (literal_value
                  (keyed_element
                    key: (literal_element
                      (identifier) @_qk)
                    value: (literal_element
                      (call_expression
                        arguments: (argument_list
                          (interpreted_string_literal) @sqs.topic))))))))
          (#eq? @sqs.direction "ReceiveMessage")
          (#eq? @_qk "QueueUrl")))))
)

;; Method receiver variant — SendMessage / ReceiveMessage inside a method body.
(method_declaration
  name: (field_identifier) @sqs.producer_fn
  body: (block
    (statement_list
      (expression_statement
        (call_expression
          function: (selector_expression
            field: (field_identifier) @sqs.direction)
          arguments: (argument_list
            (unary_expression
              operand: (composite_literal
                body: (literal_value
                  (keyed_element
                    key: (literal_element
                      (identifier) @_qk)
                    value: (literal_element
                      (call_expression
                        arguments: (argument_list
                          (interpreted_string_literal) @sqs.topic))))))))
          (#match? @sqs.direction "^(SendMessage|SendMessageBatch|ReceiveMessage)$")
          (#eq? @_qk "QueueUrl")))))
)

;; ---- BlindSpot patterns (FU-001 P3) ----
;; <expr>.MethodByName(name) — reflect-specific symbol; narrow anchor for
;; the runtime method-resolution chain (followed by .Call(...) dispatch).
((call_expression
   function: (selector_expression
     field: (field_identifier) @_m)) @blind.reflect_method_by_name
  (#eq? @_m "MethodByName"))

;; plugin.Open("file.so") — dynamic library load. The follow-up .Lookup(...)
;; symbol fetch is deferred (needs import gate to suppress non-plugin .Lookup
;; false positives, e.g. dns.Lookup); plugin.Open as a package-qualified
;; call is unambiguous.
((call_expression
   function: (selector_expression
     operand: (identifier) @_p
     field: (field_identifier) @_m)) @blind.plugin_open
  (#eq? @_p "plugin")
  (#eq? @_m "Open"))
