# cgn rename

AST-aware multi-file rename.

## Usage
```bash
cgn rename --symbol <OLD> --new-name <NEW> --dry-run
```

## Options
- `--symbol`: The current name of the symbol.
- `--new-name`: The new name to apply.
- `--dry-run`: Always start with this to see what will change.
- `--markdown`: Rename occurrences in `.md` files as well.

## Best For
- Safe refactoring of symbols across many files.
- Avoiding name collisions during rename.
