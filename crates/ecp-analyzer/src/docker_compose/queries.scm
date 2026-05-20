; Docker Compose schema-aware interpretation is done in Rust code (parser.rs),
; not via tree-sitter captures. This file is intentionally minimal — we only
; need to anchor the top-level document node so the query compiles.
(document) @document
