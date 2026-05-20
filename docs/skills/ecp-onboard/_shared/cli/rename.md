AST-aware multi-file rename

Usage: ecp rename [OPTIONS] [SYMBOL] [NEW_NAME]

Arguments:
  [SYMBOL]    The symbol name to rename (equivalent to `--symbol` flag)
  [NEW_NAME]  The new name to apply (equivalent to `--new-name` flag)

Options:
      --symbol <SYMBOL>      Named alias for the positional SYMBOL argument
      --new-name <NEW_NAME>  Named alias for the positional NEW_NAME argument
      --repo <REPO>          Repository root. Defaults to current dir
      --dry-run              Plan + verify only — do not mutate any file. Prints the diff summary to stdout
      --markdown             Also rename word-boundary occurrences in .md / .markdown / .rst / .txt documentation files. Default OFF
      --graph <GRAPH>        Path to the graph.bin file [default: .ecp/graph.bin]
  -h, --help                 Print help
