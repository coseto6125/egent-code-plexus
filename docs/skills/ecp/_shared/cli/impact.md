# ecp impact

Two modes:

1. **Symbol blast radius** — given a function / method / class, list callers and risk level.
2. **Path-literal site lookup** — given a path string (`--literal VALUE`), list every place that string appears in the graph, with sink classification (`sink:read` / `sink:write` / `sink:join` / `sink:free`).

## Usage

### Symbol blast radius (default mode)

```bash
ecp impact <SYMBOL> [--direction up] [--repo <PATH>]
```

- `<SYMBOL>`: Positional argument or `--target X`.
- `--direction`: `up` (who calls me — default), `down` (who I call), or `both`.
- `--baseline origin/main`: Compare against a branch to see impact of staged changes.
- `--kind`, `--file_path`: Filter the results.
- `--include-tests`: Include test files in the impact analysis.

### Path-literal site lookup

```bash
ecp impact --literal session_meta.json
```

Returns every file:line that embeds the exact string, with the enclosing function and the sink kind. Designed for "filename split-brain" detection: when one part of the codebase writes `session_meta.json` and another part still reads `meta.json` (the PR #357 bug class), this query surfaces both groups in one shot without composing cypher.

- `sink:read|confidence:high` — direct read (`fs::read_to_string`, `File.readText`, …)
- `sink:write|confidence:high` — direct write
- `sink:open-read` / `sink:open-write` — open-mode opens
- `sink:join|confidence:medium` — overloaded method names (`.join`, `.push`) where the receiver type can't be statically resolved
- `sink:ext-change|confidence:high` — `with_file_name` / `with_extension`
- `sink:free|confidence:high` — literal not embedded in a call (let binding, const initialiser, raw-string fixture inside macros)

`--literal` is mutually exclusive with `<NAME>` / `--target` / `--baseline`.

## Risk Levels (blast-radius mode)
- **LOW**: Few callers, strictly localized.
- **HIGH/CRITICAL**: Many callers or core library impact. **Stop and confirm with user.**
