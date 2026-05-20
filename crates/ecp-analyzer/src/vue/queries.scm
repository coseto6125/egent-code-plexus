;; Capture top-level SFC block elements so the parser can locate
;; their start positions and extract embedded script content.

;; <script> / <script setup> block — raw_text holds the JS/TS source.
(script_element
  (start_tag) @script.tag
  (raw_text)? @script.body
) @script

;; <template> block — contents not parsed; only the element span is used
;; to emit a Section node so the graph knows the file has a template block.
(template_element) @template

;; <style> block — same span-only treatment as template.
(style_element) @style
