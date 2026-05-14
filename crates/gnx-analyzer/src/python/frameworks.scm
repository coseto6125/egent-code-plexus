;; Framework-aware queries for Python (Tier 1: FastAPI subset).
;; All captures use prefix `fastapi.` to namespace them clearly.

;; FastAPI: Depends(<callable>) inside parameter defaults — captures the callable identifier.
;; Emitted as RawFrameworkRef from the enclosing function (resolved via span containment).
(call
  function: (identifier) @_depends_fn (#eq? @_depends_fn "Depends")
  arguments: (argument_list
    (identifier) @fastapi.depends.target))
