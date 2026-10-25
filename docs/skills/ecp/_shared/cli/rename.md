# ecp rename

AST-aware multi-file rename.

## Usage
```bash
ecp rename --symbol <OLD> --new-name <NEW> --dry-run
```

## Options
- `--symbol`: The current name of the symbol.
- `--new-name`: The new name to apply.
- `--dry-run`: Always start with this to see what will change.
- `--markdown`: Rename occurrences in `.md` files as well.
- `--leave-unbound`: Write the rename although some occurrences are unbound. Each unbound occurrence stays unchanged and the payload lists it. This flag never waives the ambiguity refusal or the warm-attach refusal.

## Unbound occurrences
An occurrence is unbound when it sits in a function that the graph does not link to the target.
Without `--dry-run` and without `--leave-unbound`, one unbound occurrence refuses the rename.
The command writes no file (code or markdown) and exits non-zero.
The payload has `operation: refused`, `reason: unbound_occurrences`, and `unbound` as a list of `{file, line}`.
`--dry-run` lists the unbound occurrences and exits 0.
Its headline reads `risk safe; files N; usages M` only when no file is unscanned and no occurrence is unbound.
Otherwise it reads `risk partial; files N; usages M`, then `; not scanned K` and `; unbound U` when each count is above 0.

## Best For
- Safe refactoring of symbols across many files.
- Avoiding name collisions during rename.
