# ecp impact

Calculate the blast radius of a symbol — who calls it, and how critical is the risk.

## Usage
```bash
ecp impact <SYMBOL> --direction upstream [--repo <PATH>]
```

## Options
- `<SYMBOL>`: Positional argument or `--target X`.
- `--direction`: `upstream` (who calls me), `downstream` (who I call), or `both`.
- `--baseline origin/main`: Compare against a branch to see impact of staged changes.
- `--kind`, `--file_path`: Filter the results.
- `--include-tests`: Include test files in the impact analysis.

## Risk Levels
- **LOW**: Few callers, strictly localized.
- **HIGH/CRITICAL**: Many callers or core library impact. **Stop and confirm with user.**
