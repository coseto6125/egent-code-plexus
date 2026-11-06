# ecp impact

Two modes:

1. **Symbol blast radius** — given a function / method / class, list every caller (upstream) or callee (downstream) with file:line and caller counts.
2. **Path-literal site lookup** — given an exact path string (`--literal VALUE`), list every place that string appears in the graph, with sink classification (`sink:read` / `sink:write` / `sink:join` / `sink:free`).

## Usage

### Symbol blast radius (default mode)

```bash
ecp impact <SYMBOL> [--direction up] [--repo <PATH>]
```

- `<SYMBOL>`: Positional argument or `--target X`.
- `--direction`: `up` (who calls me — default), `down` (who I call), or `both`.
- `--baseline origin/main`: Compare against a branch to see impact of staged changes.
- `--kind`, `--file`: Select the target declaration, not the results. `--kind` takes one kind and applies before `--file`. `--file` ranks a path that equals the value, then one that ends with it, then one that contains it. An ambiguous name returns candidates that you can re-run. `--file_path` is an alias of `--file`.
- `unexplained_refs`: An upstream walk scans the repository text when the target has at most 2 non-test direct callers or shares its name with another definition. The field lists the whole-word occurrences of the name that no graph edge explains (at most 20 sites, `{path, line, form, enclosing}`; `form` is `bare` / `member` for a call, `reference` for any other use, such as a function passed as a value or a template). `total` counts every occurrence, and `total: 0` means the scan found none. Read the sites before you treat a short caller list as complete; they are text matches, never counted as callers. The scan skips vendored code, build output and documents, and searches every language. `--ambiguous-callers` forces the scan at any caller count. `--batch` scans only with that flag and only a colliding name.
- An ambiguous name exits non-zero and lists at most 5 `candidates`, most-called first, each with its re-runnable `target`, `callers` and `top_callers`. Candidates under `node_modules/`, `vendor/`, `.venv/` or `site-packages/` are dropped unless all are there; `more` and `more_command` (`ecp find <name> --all`) cover the rest. Pick one and re-run its `target`.
- `blind_spot_warning` counts blind spots only where this answer has evidence: upstream, in the files of the `unexplained_refs` sites; downstream, in the target's own file. `files` is always an array.
- Test callers are listed by default, each entry tagged `test: true`. A test caller sits in a test file or carries a test attribute (`#[test]`, `@Test`, `[Fact]`, a pytest decorator); a test-like name alone is not one. `--exclude-tests` drops them and reports `hidden_test_callers: N`.
- Every row carries `requires_verification`. It is `true` when a hop on the row's path is heuristic or has confidence below 0.7. The tag reflects the best path of the shortest depth: fewest tags first, then a deterministic edge over a heuristic one, then the higher confidence, so a row reached by a tagged `Calls` edge and a heuristic edge sits in the deterministic list. Read a `true` row as a lead: open the call site before you act on it.
- Every row carries `ownerClass` and `lexicalParent`. `ownerClass` is the stored owner: the class of a method, null for a nested fn or closure. `lexicalParent` is the enclosing chain of a nested fn (`outer`, `K::m`), else the stored owner.
- `--test-coverage` counts only verified (untagged) test callers as coverage. Tagged test callers appear in `unverified_test_callers`. A symbol whose only callers are tagged test callers is `uncovered`, not `orphan`.

### Path-literal site lookup

```bash
ecp impact --literal session_meta.json
```

Returns every file:line that embeds the exact string, with the enclosing function and the sink kind. Designed for "filename split-brain" detection: when one part of the codebase writes `session_meta.json` and another part still reads `meta.json` (the PR #357 bug class), query each suspected literal separately and compare the two result sets.

`--literal` does not do Levenshtein or other fuzzy clustering; use exact values.

- `sink:read|confidence:high` — direct read (`fs::read_to_string`, `File.readText`, …)
- `sink:write|confidence:high` — direct write
- `sink:open-read` / `sink:open-write` — open-mode opens
- `sink:join|confidence:medium` — overloaded method names (`.join`, `.push`) where the receiver type can't be statically resolved
- `sink:ext-change|confidence:high` — `with_file_name` / `with_extension`
- `sink:free|confidence:high` — literal not embedded in a call (let binding, const initialiser, raw-string fixture inside macros)

`--literal` is mutually exclusive with `<NAME>` / `--target` / `--baseline`.

### Path-literal split-brain scan

```bash
ecp impact --literal-coherence --format json
```

Scans all PathLiteral nodes and emits likely filename split-brain candidate pairs. A pair is reported only when the literals have the same extension, similar basenames, nearby source directories, and separated access patterns (one read-only, one write-only). Use this before guessing candidate pairs manually.

## Reading the blast radius

`impact` returns the raw caller / callee set — it does not assign a risk label. Use the result to size the blast radius: a large upstream caller count, or callers in core / widely-imported modules, means a wide one. Whether a breaking change needs the user's sign-off is the breaking-change rule in `ECP.md`. Caller counts are a **lower bound** (the resolver suppresses ambiguous bare calls to common names); a suspiciously low count warrants a `grep` cross-check before trusting it.
