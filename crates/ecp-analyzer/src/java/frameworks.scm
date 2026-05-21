;; Framework-aware queries for Java (Tier 2: Spring subset + Redis pub/sub).

;; Spring @Autowired field injection — capture enclosing class name and
;; the injected field's type. Confidence 0.8, reason "spring-autowired".
;;
;; Pattern: class { @Autowired private SomeType field; }
(class_declaration
  name: (identifier) @spring.autowired.class
  body: (class_body
    (field_declaration
      (modifiers
        (marker_annotation
          name: (identifier) @_autowired_kw
          (#eq? @_autowired_kw "Autowired")))
      type: (type_identifier) @spring.autowired.target)))

;; ---- Kafka Java (T5-5) ----
;; Covers org.apache.kafka (KafkaProducer.send / KafkaConsumer.subscribe) and
;; org.springframework.kafka (KafkaTemplate.send).
;; Import gate (org.apache.kafka, org.springframework.kafka) is enforced by
;; KAFKA_JAVA.import_gate — these queries fire on syntax alone; the extractor
;; filters by import at runtime.
;;
;; Anchored to `method_declaration` to co-capture the enclosing method name.
;; Variable topic args produce no capture (no fabrication).

;; Apache Kafka producer: producer.send(new ProducerRecord<>("topic", ...))
;; Captures the first string_literal argument to the ProducerRecord constructor.
(method_declaration
  name: (identifier) @kafka.java.fn
  body: (block
    (_
      (method_invocation
        name: (identifier) @kafka.java.direction
        (#eq? @kafka.java.direction "send")
        arguments: (argument_list
          (object_creation_expression
            type: (_) @_rec_type
            (#match? @_rec_type "^ProducerRecord")
            arguments: (argument_list
              . (string_literal) @kafka.topic)))))))

;; Spring Kafka producer: kafkaTemplate.send("topic", ...)
;; Captures the first string_literal argument to `send`.
(method_declaration
  name: (identifier) @kafka.java.fn
  body: (block
    (_
      (method_invocation
        name: (identifier) @kafka.java.direction
        (#eq? @kafka.java.direction "send")
        arguments: (argument_list
          . (string_literal) @kafka.topic)))))

;; Apache Kafka consumer subscribe: consumer.subscribe(Arrays.asList("topic",...))
;; Captures the first string_literal in the asList argument list.
(method_declaration
  name: (identifier) @kafka.java.fn
  body: (block
    (_
      (method_invocation
        name: (identifier) @kafka.java.direction
        (#eq? @kafka.java.direction "subscribe")
        arguments: (argument_list
          (method_invocation
            name: (identifier) @_aslist
            (#eq? @_aslist "asList")
            arguments: (argument_list
              . (string_literal) @kafka.topic)))))))

;; Spring @RestController / @Controller class with @GetMapping / @PostMapping /
;; @PutMapping / @DeleteMapping / @PatchMapping / @RequestMapping methods.
;;
;; Safety guard: enclosing class MUST carry @RestController or @Controller —
;; the predicate `#match?` on @_rc enforces this; methods inside a plain
;; class are not captured even if they have @GetMapping.
;;
;; @Controller / @RestController may appear as marker_annotation (no args) or
;; annotation (with args, e.g. `@RequestMapping("/api")` siblings are allowed
;; in the modifiers block). Verb annotations are typically `annotation` form
;; (e.g. `@GetMapping("/users/{id}")`) but we also accept marker form.
(class_declaration
  (modifiers
    [(marker_annotation name: (identifier) @_rc)
     (annotation name: (identifier) @_rc)]
    (#match? @_rc "^(RestController|Controller)$"))
  name: (identifier) @spring.route.class
  body: (class_body
    (method_declaration
      (modifiers
        [(marker_annotation name: (identifier) @_verb)
         (annotation
           name: (identifier) @_verb
           arguments: (annotation_argument_list
             [(string_literal) @spring.route.path (MISSING) @spring.route.path])?)]
        (#match? @_verb "^(GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|RequestMapping)$"))
      name: (identifier) @spring.route.handler)))

;; ---- Redis Java pub/sub (T5-29) ----
;; Covers spring-data-redis, Jedis, and Lettuce Core under one config slice.
;; Import gate (org.springframework.data.redis / redis.clients.jedis /
;; io.lettuce.core) is enforced by REDIS_JAVA.import_gate — these queries fire
;; on syntax alone; the extractor filters by import at runtime.
;;
;; `redis.direction` captures the method name so `classify_redis_direction` can
;; distinguish Subscribe from Publish.
;;
;; Topic literal: the first positional string literal arg to the call.
;; Variable channel args produce no `redis.topic` capture → no RawEventTopic.
;;
;; Anchored to `method_declaration` to co-capture the enclosing method name.

;; spring-data-redis: redisTemplate.convertAndSend("channel", message) — Publish.
(method_declaration
  name: (identifier) @redis.fn
  body: (block
    (_
      (method_invocation
        name: (identifier) @redis.direction (#eq? @redis.direction "convertAndSend")
        arguments: (argument_list
          . (string_literal) @redis.topic)))))

;; Jedis / Lettuce: obj.publish("channel", msg) — Publish.
(method_declaration
  name: (identifier) @redis.fn
  body: (block
    (_
      (method_invocation
        name: (identifier) @redis.direction (#eq? @redis.direction "publish")
        arguments: (argument_list
          . (string_literal) @redis.topic)))))

;; Lettuce: commands.subscribe("channel") — Subscribe.
(method_declaration
  name: (identifier) @redis.fn
  body: (block
    (_
      (method_invocation
        name: (identifier) @redis.direction (#eq? @redis.direction "subscribe")
        arguments: (argument_list
          . (string_literal) @redis.topic)))))

;; Lettuce: commands.psubscribe("pattern.*") — Subscribe (glob pattern).
;; Pattern strings are glob expressions; stored as-is in topic_literal.
(method_declaration
  name: (identifier) @redis.fn
  body: (block
    (_
      (method_invocation
        name: (identifier) @redis.direction (#eq? @redis.direction "psubscribe")
        arguments: (argument_list
          . (string_literal) @redis.topic)))))

;; Lettuce reactive API: commands.pSubscribe("pattern.*") — Subscribe (camelCase).
(method_declaration
  name: (identifier) @redis.fn
  body: (block
    (_
      (method_invocation
        name: (identifier) @redis.direction (#eq? @redis.direction "pSubscribe")
        arguments: (argument_list
          . (string_literal) @redis.topic)))))
