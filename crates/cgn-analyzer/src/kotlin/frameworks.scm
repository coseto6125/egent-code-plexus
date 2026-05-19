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
