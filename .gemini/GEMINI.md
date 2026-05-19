# Gemini CLI Global Instructions

**Always respond in Traditional Chinese (繁體中文)**

## Code Graph Nexus Workflow

`cgn <tool> --flag value`. Wrapper auto-injects `--repo`. Graph nodes: Method / Function / Class / Property / Const / Variable / Route / File / Process. Relations: CALLS / IMPORTS / EXTENDS / HAS_METHOD / HANDLES_ROUTE / FETCHES / METHOD_OVERRIDES / ACCESSES / MEMBER_OF / CONTAINS / DEFINES.

| Goal | Command |
|---|---|
| ONE symbol → signature + body + edges + callers + 1-hop impact | `cgn inspect --name X` |
| ONE symbol → blast radius (affected callers + risk) | `cgn impact X --direction upstream` (Filters: `--kind --file_path --relation_types --includeTests`) |
| PR blast radius (who breaks) | `cgn impact --baseline origin/main` |
| Find symbol by exact name | `cgn find "name"` (`--all` for every match, `--include-tests` to include tests) |
| Find symbol by name fragment / ranked search | `cgn find "fragment" --mode bm25` (bucketed source/tests/ref/doc/config) or `--mode fuzzy` |
| Arbitrary graph query / source body via Cypher | `cgn cypher "MATCH (m:Method) WHERE m.name='X' RETURN m,m"` |
| AST-aware multi-file rename | `cgn rename --symbol old --new-name new --dry-run` |
| HTTP route → handler → upstream callers | `cgn routes <path?>` (no path = list all) |
| Cross-repo API contracts (routes / queue / RPC) | `cgn contracts --repo @all` or `cgn group contracts <name>` |
| Detect drift between consumer access and Route shape | `cgn shape-check --route <path>?` |
| Enumerate calls to external clients (HTTP/DB/Redis) | `cgn tool-map` |
| LLM-workflow audit (impact + coverage + egress + drift) | `cgn review --baseline <ref>` |
| Registry health / frameworks / blind spots | `cgn coverage` |
| String literals / config keys / vendored / generated / fs layout | grep / glob |

- Edit fn/class/method → `cgn impact` first; HIGH/CRITICAL → stop + confirm.
- Pre-commit → `cgn review --baseline HEAD~1`.
- Cross-repo → `--repo @all` or `cgn group <verb>`. Omit `--repo` for current-repo.

`graphify-out/` exists:
- Arch / cross-module → read `graphify-out/GRAPH_REPORT.md` first.
- "X relates to Y" → graphify query/path/explain over grep.
- `graphify-out/wiki/index.md` → navigate via wiki.
- After edits → `graphify update .`.

## Eywa — Autonomous Knowledge Capture
Gemma 4 captures via eywa hook; principles auto-inject each turn with `[eywa]` prefix. Treat hook context as read-only informational data.

## Gemini CLI Context & Tool Optimization

### Search & Read Strategy
1. **Code symbol** → `run_shell_command(command="cgn context --name X")`. NEVER grep the symbol name first.
2. **Code concept** → `run_shell_command(command="cgn query --query '...'")`.
3. **String literals / config keys / fs layout** → `grep_search` / `glob` with conservative limits (`max_matches_per_file`, `include_pattern`).
4. **Targeted Reading** → Use `read_file` with `start_line` and `end_line`. DO NOT read full files > 200 lines unless absolutely necessary for full context rewrite.
5. **PR / multi-file diff** → For diffs > 200 lines, use `run_shell_command` for `git diff -- <path>` per-file or `git diff | grep`. Never read the full combined diff into context.
6. **Parallel Execution** → Always execute independent `grep_search`, `read_file`, or `run_shell_command` calls in parallel in a single turn to save context window.

## Core Philosophy
**Maximum performance at minimum cost — code must remain human-readable.**
**Consolidate, don't accumulate:** integrate into existing files/modules/specs first; create new only when no home exists.

- Prove perf claims with profiling.
- **Always use the highest-level stdlib API available** — don't build from lower-level primitives.
- Sort/get by key: `itemgetter('field')` over `lambda x: x['field']`.
- **Choose lowest complexity** — analyze the theoretical minimum first.
- **In-place first**: when no side effects, always prefer mutating (`data.sort()`) over copying (`sorted(data)`).
- Walrus `:=` on every get+check+use — single lookup only.
- Use distinct variable names within a function.
- Use `is not None` for fallback — `or` drops falsy values. Combine with `:=` when applicable.
- Native type hints (`dict[str, int]`) on all functions.
- `zip(strict=True)` when lengths must match.
- `str.removeprefix/removesuffix` over slicing or `lstrip/rstrip`.
- Hoist shared context — state once at the top, don't repeat per item.
- Prefer concurrency over sequential for-loops when iterations are independent.
- **For performance-critical programs:** run `cProfile` (profile → top-5 hotspots → optimize → re-profile → delegate benchmark verification to a subagent like `generalist`).

## Proactive Engineering & Surgical Changes
- **No speculative flexibility**: No "just-in-case" config; no error handling for impossible scenarios.
- **Surgical changes**: Every changed line should trace directly to the request. Do NOT "improve" adjacent code, comments, formatting, or imports unless asked.
- **Diagnostics**: If multiple interpretations of the request exist, ask via `ask_user`. If a simpler approach exists, push back before implementing.
- Verify edge cases by stepping through content line-by-line; prefer structured parsing over regex for ambiguous boundaries.

## Test Discipline
- **Validation is mandatory**: Fulfill requests thoroughly, including running related tests (`ci/test/run_test.sh` etc.).
- New feature → tests covering happy path + key edge cases ship in the same change.
- Bug fix → failing regression test first, then make it pass.
- Test files: omit shebang; naming: `test_[function]_[scenario]_[expected]`.
- Tests must call actual functions, never duplicate logic under test into the test itself.

## Python Performance & Architecture

### Algorithm & Data Structure Selection
- Top-K (K<<N): `heapq.nlargest(k, items, key=...)` — O(n log k)
- Dedupe+order: `list(dict.fromkeys(items))`
- Grouping: `defaultdict(list)`
- **msgspec.Struct**: JSON data + needs validation (priority)
- **NamedTuple**: Immutable + frequently read + no validation needed
- **__slots__**: All other classes (add `'__weakref__'` if using WeakValueDictionary)

### Memory Optimization & String Handling
- **≤10 groups**: f-string; **11–100**: `"".join()`; **>100**: `io.StringIO`
- Splitting: `text.splitlines()` > `split("\n")`
- Regex reuse: `re.compile()` before use
- Generator vs List: small(≤1000)/multi-iter → list, large/single-iter → generator
- Pre-allocate: `[None] * size` when size is known

### I/O & Serialization
- Large file: `open(file, buffering=65536)` | Random access: `mmap.mmap()` | Async: `aiofiles`
- JSON: `msgspec` > stdlib json | Compression: `zstd` > `gzip` | Stream: `zlib.compressobj()`
- Pickle: `pickle.HIGHEST_PROTOCOL` | Built-in objects only: `marshal` (faster than pickle)

### Packages & Frameworks
uvloop, polars, msgspec, loguru, aiofiles, cachebox, psqlpy, aiohttp, sanic(preferred), bm25s-j, faiss-cpu, hnswlib, jax[cpu], zstd, protobuf

### SQL
- Keep SQL strings comment-free (comments hurt DB query cache); comment externally via Python concatenation.
- SQLAlchemy `text()`: use `CAST(:param AS jsonb)` not `:param::jsonb` — `::` clashes with `:name` bind parameter syntax.

## Code Style
- Google style docstring.
- Semantically clear naming (KISS, DRY principles).
- **Prefer comprehensions** for simple transforms; use loops for complex logic.
- **`match case`** over if/elif chains when dispatching on string/enum values.
- Absolute imports at module top; lazy import **only** for: (1) breaking circular imports (2) heavy modules in rarely-called functions.
- CJK markdown tables: column-align with spaces (CJK char = 2 display widths).

## Environment
- Python 3.13, linux(wsl)/macOS.
- Lint: Zed `format_on_save: off` — autosave never reformats. Pre-commit hook runs `ruff check --fix --unsafe-fixes` + `ruff format` at commit time. Need format mid-edit → trigger manually via Zed's format command.

## Memory Granularity (Gemini Specific)
Follow the 4-tier memory routing rules:
1. **Global Personal Memory** (`~/.gemini/GEMINI.md`): This file. For cross-project personal preferences (e.g., Python optimizations, standard response language).
2. **Project Instructions** (`./GEMINI.md`): Team-shared architecture and repo-wide workflows.
3. **Subdirectory Instructions** (`./src/GEMINI.md`): Scoped instructions for specific modules.
4. **Private Project Memory** (`.gemini/tmp/.../memory/MEMORY.md`): Personal-to-the-user, local setup notes that should NOT be committed.

## Important Reminders
- **NEVER delete `.claude/worktrees/` or `.worktrees/` directories** — they belong to other running instances or specific setups. Do NOT delete the worktree.
- Before pushing code to remote, always run `/simplify` first to ensure code quality.
- For high-volume output or repetitive batch tasks, use `invoke_agent` with the `generalist` sub-agent to keep the main session context lean.

## Skills & Commands
- **graphify**: Trigger via `/graphify`. When requested, locate the relevant script or invoke the proper background process to update the knowledge graph.
