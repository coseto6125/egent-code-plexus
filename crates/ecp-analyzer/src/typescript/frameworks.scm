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
