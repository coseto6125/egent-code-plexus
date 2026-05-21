;; Framework-aware queries for Kotlin (Ktor routing DSL).
;;
;; Ktor exposes route declarations as statement-level call expressions inside
;; a `routing { ... }` lambda:
;;     get("/users") { call.respondText("ok") }
;;
;; AST shape (tree-sitter-kotlin 0.4):
;;   call_expression
;;     simple_identifier (text == "get"/"post"/"put"/"delete"/"patch")
;;     call_suffix
;;       value_arguments
;;         value_argument
;;           string_literal
;;             string_content                     ; <- route path text
;;       annotated_lambda? / lambda_literal       ; <- handler body
;;
;; Emit one capture per supported verb so the parser can pick the right
;; `reason = ktor-route-<verb>` without regex alternation in Rust code.

(call_expression
  (simple_identifier) @_ktor_get_kw (#eq? @_ktor_get_kw "get")
  (call_suffix
    (value_arguments
      (value_argument
        (string_literal
          (string_content) @ktor.route.path))))) @ktor.route.get

(call_expression
  (simple_identifier) @_ktor_post_kw (#eq? @_ktor_post_kw "post")
  (call_suffix
    (value_arguments
      (value_argument
        (string_literal
          (string_content) @ktor.route.path))))) @ktor.route.post

(call_expression
  (simple_identifier) @_ktor_put_kw (#eq? @_ktor_put_kw "put")
  (call_suffix
    (value_arguments
      (value_argument
        (string_literal
          (string_content) @ktor.route.path))))) @ktor.route.put

(call_expression
  (simple_identifier) @_ktor_delete_kw (#eq? @_ktor_delete_kw "delete")
  (call_suffix
    (value_arguments
      (value_argument
        (string_literal
          (string_content) @ktor.route.path))))) @ktor.route.delete

(call_expression
  (simple_identifier) @_ktor_patch_kw (#eq? @_ktor_patch_kw "patch")
  (call_suffix
    (value_arguments
      (value_argument
        (string_literal
          (string_content) @ktor.route.path))))) @ktor.route.patch

;; ---- Kafka Kotlin (T5-5, JVM symmetry) ----
;; Covers org.apache.kafka (send/subscribe) and org.springframework.kafka (send).
;; Import gate is enforced by KAFKA_KOTLIN.import_gate — queries fire on syntax
;; alone; the extractor filters by import at runtime.
;;
;; Anchored to `function_declaration` to co-capture the enclosing function name.
;; Variable topic args produce no capture (no fabrication).
;;
;; tree-sitter-kotlin 0.4.0 grammar notes:
;;   function_declaration: (simple_identifier) @fn (function_body ...)
;;   function_body directly contains call_expression in queries (statements wrapper
;;   is transparent to tree-sitter pattern matching)
;;   call_expression: (navigation_expression ...) (call_suffix ...)
;;   navigation_expression: (simple_identifier) (navigation_suffix (simple_identifier))
;;   Method name is inside navigation_suffix, NOT directly in navigation_expression.
;;   Arguments: call_suffix → value_arguments → value_argument
;;   String literals: string_literal (includes quotes) → captured as @kafka.topic
;;   (strip_string_delimiters in extract.rs removes the quotes)

;; Apache Kafka producer: producer.send(ProducerRecord("topic", ...))
;; Captures the first string_literal argument to the ProducerRecord constructor call.
;; IMPORTANT: In tree-sitter-kotlin 0.4.0, function_body wraps statements in a
;; `statements` node — queries must include `statements` explicitly.
(function_declaration
  (simple_identifier) @kafka.kotlin.fn
  (function_body
    (statements
      (call_expression
        (navigation_expression
          (navigation_suffix
            (simple_identifier) @kafka.kotlin.direction
            (#eq? @kafka.kotlin.direction "send")))
        (call_suffix
          (value_arguments
            (value_argument
              (call_expression
                (simple_identifier) @_kotlin_rec_type
                (#match? @_kotlin_rec_type "^ProducerRecord")
                (call_suffix
                  (value_arguments
                    . (value_argument
                      (string_literal) @kafka.topic)))))))))))

;; Spring Kafka: template.send("topic", msg)
;; Captures the first string_literal directly in the argument list.
(function_declaration
  (simple_identifier) @kafka.kotlin.fn
  (function_body
    (statements
      (call_expression
        (navigation_expression
          (navigation_suffix
            (simple_identifier) @kafka.kotlin.direction
            (#eq? @kafka.kotlin.direction "send")))
        (call_suffix
          (value_arguments
            . (value_argument
              (string_literal) @kafka.topic)))))))
