# ecp pattern

Syntax-pattern search over source files — the statement shapes the graph does not hold.

The graph stores declarations, so "which `try` swallows its exception" or
"which request omits a timeout" has no node to match. `ecp pattern` answers
those by running an ast-grep pattern over the files the graph already knows.

## Usage
```bash
ecp pattern -p 'requests.get($URL)' --lang py
ecp pattern -p 'try:
    $$$BODY
except $$$E:
    pass' --lang py
ecp pattern -p 'json.loads($X)' --callers-of load_config
```

## Options
- `-p` / `--pattern`: the pattern. `$NAME` captures one node, `$$$NAME` a run of them.
- `--callers-of`: scan only the files holding a symbol's callers. Accepts the
  same `Owner.method` qualification as `ecp rename`.
- `--lang`: restrict to one extension, without the dot (`py`, `ts`).
- `--limit`: cap on reported matches (default 200; `0` means no cap).

## Best For
- Statement shapes: swallowed exceptions, missing timeouts, a call form to migrate.
- Scoping a pattern by graph reachability — `--callers-of` is the part a bare
  `ast-grep` run cannot express, because it matches syntax, not binding.

## Notes
- A pattern is language-specific. Without `--lang`, an extension whose compile
  fails is skipped; the run fails only when every supported language rejects
  the pattern, and the message then carries the parser's own diagnostic.
- With `--lang`, the pattern is compiled before any file is read, so a pattern
  that language rejects is an error even when no file of that language is in
  scope. An empty result there means the pattern is valid and matched nothing.
- `--callers-of` naming a symbol the graph does not carry is an error, not an
  empty result.
- Supported: Python, Rust, Go, Ruby, Kotlin, C#, PHP, Swift, C, C++, Java,
  TypeScript/TSX, JavaScript. An unsupported extension is skipped silently.
