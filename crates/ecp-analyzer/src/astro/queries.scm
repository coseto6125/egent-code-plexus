;; Capture the top-level SFC regions so the parser can locate them.

;; Frontmatter block — `frontmatter_js_block` holds the TypeScript source
;; between the two `---` fences.
(frontmatter
  (frontmatter_js_block)? @frontmatter.body
) @frontmatter

;; Template region — the body after frontmatter; we emit a single Section node
;; spanning all non-frontmatter, non-script, non-style document children.
;; tree-sitter-astro puts the full file under `document`, so we use a sentinel
;; query here — template extent is computed from document bounds in Rust.

;; <style> blocks — span-only, contents not parsed.
(style_element) @style

;; Client-side <script> blocks — span-only, contents not parsed.
(script_element) @script
