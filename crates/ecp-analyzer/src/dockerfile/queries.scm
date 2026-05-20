;; FROM instructions — treat each base image as an import dependency
(from_instruction
  (image_spec
    name: (image_name) @import.source)) @import

;; ENTRYPOINT — the container's primary entry point, emitted as a Function node
(entrypoint_instruction) @entrypoint

;; CMD — fallback command / entry point, emitted as a Function node
(cmd_instruction) @cmd

;; ENV — environment variable declarations, emitted as Const nodes
(env_instruction
  (env_pair
    name: (unquoted_string) @const.name)) @const

;; ARG — build-time argument declarations, emitted as Const nodes
(arg_instruction
  (arg_pair
    name: (unquoted_string) @arg.name)) @arg
