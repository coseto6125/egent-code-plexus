AST-aware multi-file rename

Usage: ecp rename [OPTIONS] [SYMBOL] [NEW_NAME]

Arguments:
  [SYMBOL]    The declaration to rename (equivalent to `--symbol` flag): `name`, `Owner.name`, `::name` for a declaration with no owner, or `name@LINE`
  [NEW_NAME]  The new name to apply (equivalent to `--new-name` flag)

Options:
      --symbol <SYMBOL>         Named alias for the positional SYMBOL argument
      --new-name <NEW_NAME>     Named alias for the positional NEW_NAME argument
      --repo <REPO>             Repository root. Defaults to current dir
      --dry-run                 Plan + verify only — do not mutate any file. Prints the diff summary to stdout
      --markdown                Also rename word-boundary occurrences in .md / .markdown / .rst / .txt documentation files. Default OFF
      --show-heuristic-mirrors  Expand the heuristic mirror list in the output. When unset, only the count is shown. Tier shows the `unresolved` sentinel until T4-7 populates real tier/check values
      --file <FILE>             Select the declaration by its file path when the target matches several. A path equal to the value wins, then a path that ends with `/<value>`, then a path that contains it
      --kind <KIND>             Select the declaration by its kind when the target matches several: function, method, class, struct, and the other node kinds
      --leave-unbound           Write the rename although some occurrences sit in functions the graph does not link to the target. Those occurrences stay unchanged and the payload still lists them. It never waives the ambiguity refusal or the warm-attach refusal
      --graph <GRAPH>           Path to the graph.bin file [default: .ecp/graph.bin]
  -h, --help                    Print help
