;; Framework-aware queries for Java (Tier 2: Spring subset).

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
         (annotation name: (identifier) @_verb)]
        (#match? @_verb "^(GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|RequestMapping)$"))
      name: (identifier) @spring.route.handler)))
