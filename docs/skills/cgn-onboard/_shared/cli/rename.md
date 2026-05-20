AST-aware multi-file rename

Usage: cgn rename [OPTIONS] --symbol <SYMBOL> --new-name <NEW_NAME>

Options:
      --symbol <SYMBOL>      The symbol name to rename (e.g. `old_name`)
      --new-name <NEW_NAME>  The new name to apply
      --repo <REPO>          Repository root. Defaults to current dir
      --dry-run              Plan + verify only — do not mutate any file. Prints the diff summary to stdout
      --markdown             Also rename word-boundary occurrences in .md / .markdown / .rst / .txt documentation files. Default OFF
      --graph <GRAPH>        Path to the graph.bin file [default: .cgn/graph.bin]
  -h, --help                 Print help
