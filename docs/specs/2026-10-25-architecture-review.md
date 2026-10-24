# Architecture review: whole repo, 2026-10-25

Thirteen reviewers each read one subsystem of ecp, read-only, against `main` at `8868d659`. One more measurement covers the 14 `parse_file` bodies. Each reviewer ranks its findings by what the finding does to the answer of the agent that calls ecp. This page is the evidence inventory: every finding has its id, its evidence and its status. The structural plan (areas 1-11) is in [the architecture direction page](2026-10-25-architecture-direction.md).

| Tier | Meaning |
|---|---|
| T1 | The agent gets a wrong answer, or a right answer with no marker that it is stale or incomplete. |
| T2 | The query is slower than the latency target. |
| T3 | The output holds noise that costs context. |
| T4 | A change costs many edits across languages or files. |
| T5 | The finding affects maintenance only. |

## Status words

| Status | Meaning |
|---|---|
| re-run | The main session ran the command itself and saw the output in the finding. |
| reviewer repro | The reviewer ran a command and quoted the output. The main session did not re-run it. |
| code read | The finding rests on reading the code. No command reproduced it. |

The last column, "goes to", holds an area number from the direction page, or `fix` (a small local fix), `measure` (measure before acting), `filter` (LLM-utility filter deletion candidate), or `none` (the area map does not place the finding).

## Subsystems

| subsystem | scope (dirs) | verdict | T1 count | finding ids | areas touched |
|---|---|---|---|---|---|
| CLI commands and target resolution | `crates/ecp-cli/src` (`main.rs`, `cli.rs`, `engine.rs`, `output.rs`, `commands/*`) | Seven name resolvers disagree; the stale caveat and the overlay reach only part of the commands. | 7 | F1, F2, F4, F5, F6, F9, F11, F7, F10, F12, Q3, F8, Q4-1, Q4-2, Q4-3, Q4-4, Q4-5 | 1, 5, fix |
| Types and session view | `crates/ecp-core/src/analyzer`, `crates/ecp-core/src/session`, `crates/ecp-analyzer/src/*/receiver_types.rs`, `crates/ecp-cli/src/session` | Resolver half is unified; most graph-reading command files ignore the overlay; extraction is 14 copies. | 4 | B1, B4, A2, A3, A5, A4, A6, B2, B3, Q6-1, Q6-2 | 5, 10, fix |
| Flow | `crates/ecp-analyzer/src/flow`, `crates/ecp-cli/src/commands/flow*`, `shape_check.rs` | Own relative-only import resolver; whole-corpus cost per query; global truncation latch. | 5 | F1, F1b, F2, F3, F4, F5, F6, F7, F8, F9 | 4, fix, measure |
| Cypher | `crates/ecp-core/src/cypher`, `crates/ecp-cli/src/commands/cypher.rs`, `graph_csr.rs` | Multi-pattern, WITH, EXISTS, DISTINCT, ORDER BY and string literals give wrong answers with no error. | 14 | C1, C2, C3, C4, C5, C6, C7, C8, C9, C10, C11, C12, C13, C14, C15, C16, C17, C18 | 9 |
| Agent integration (hooks, MCP, skill) | `crates/ecp-cli/src/commands/hook`, `crates/ecp-mcp`, `crates/ecp-cli/src/admin/host_integration` | MCP drops the stale caveat; the hook has no distance gate; the tool list exposes uninstall. | 5 | A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, T5-1, T5-2 | 5, 8, fix, filter |
| Frameworks and routes | `route_detector.rs`, `entry_points.rs`, `framework_helpers.rs`, `event_topic/`, `fetch_shape/`, `frameworks.scm` files, `commands/routes.rs`, `commands/contracts.rs` | Wrong HTTP verbs, three unreconciled route producers, no prefix composition, a stub `contracts`. | 7 | F1, F2, F3, F4, F5, F6, F7, F8, F9, F10 | 7, 8, fix |
| Freshness and incremental | `crates/ecp-cli/src/auto_ensure.rs`, `reanalyze.rs`, `incremental.rs`, `git/`, `session/`, `admin/` | Subdirectory runs, deletions and 18 of 37 providers are invisible to the overlay. | 8 | F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, parity gap | 5, 6, 8, 11, fix, measure, filter |
| Index pipeline | `crates/ecp-cli/src/build`, `commands/admin/index.rs`, `search.rs`, `crates/ecp-analyzer/src/resolution` | One slot gets two ignore policies; six path tables; tantivy loss is silent. | 7 | F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, T5-1, T5-2, T5-3, T5-4 | 3, 5, 6, 7, fix, measure, filter |
| Analyzer post-parse | `crates/ecp-analyzer/src/post_process`, `function_meta`, `identifier_finder`, `sql_literal`, `calls.rs`, `indirect_dispatch.rs` | Edges attach to the first same-named symbol; Overrides match by name; `is_test` is missing for 12 languages. | 9 | F1, F1 owner spelling, F2, F3, F4, F5, F6, F7, F8, F9, F10, M-1, M-2, M-3, M-4, M-5, M-6 | 2, 4, 5, 6, 7, 10, measure, filter |
| parse_file bodies | `crates/ecp-analyzer/src/<14 languages>/parser.rs` | 40.0% of 7,899 body lines is the same logic in three or more languages. | 0 | D, helpers, commits | 10 |
| Core graph | `crates/ecp-core/src` (`graph.rs`, `graph_assembly.rs`, `registry/`, `algorithms/`, `file_category.rs`) | Cfg twins become tombstones; `Language` lacks Vue and Svelte; process traces use weak edges. | 6 | F1, F2, F3, F4, F6, F8, F9, F5, F11, F7, F10, F12, F13, F14 | 2, 6, fix, filter |
| Tests | `crates/*/tests`, `scripts/parity`, `scripts/benchmark` | The largest parity file uses a loose key; fixtures only prove rejection; no 14-language gate. | 3 | T-1, T-2, T-3, T-4, T-5, T-6, T-7 | 6, 10, 11 |
| DSL providers | DSL and config provider dirs (`astro` to `zig`) in `crates/ecp-analyzer/src`, `sfc_common.rs`, `crates/ecp-cli/src/provider_registry.rs`, `walker_filter.rs` | The overlay has 19 of 37 providers; OpenAPI YAML never routes; Vue and Svelte emit no calls. | 8 | F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13 | 2, 6, 10, fix |
| Secondary commands | `crates/ecp-cli/src/commands/{group,review,diff,dev,rename,pattern,schema}`, `output.rs`, `peer/` | `group sync` makes no cross-repo link; `rename` renames every same-named symbol; `diff` stashes the user tree. | 12 | G1, RN1, D1, O1, DV1, G2, G3, G4, R1, R2, R3, D2, D3, SC1, O2, P1, P2, PT1 | 5, 8, fix, filter |

## Findings

Each table lists every T1-T4 finding of its report. T5 findings have one short row each.

### CLI commands and target resolution

| id | tier | problem | evidence | status | goes to |
|---|---|---|---|---|---|
| cli F1 | T1 | Seven name resolvers disagree. `ecp find Engine.load` finds nothing, and `ecp rename base_visible` reports no occurrences, but `inspect` and `impact` resolve both. | `ecp find Engine.load --repo . --format json` returns `{"found":false,"matches":[],"returned":0,"status":"success"}`. `ecp rename base_visible base_visible_zz --dry-run --repo .` prints `No occurrences of "base_visible" found.` (method at `crates/ecp-core/src/session/merged.rs:136`). | re-run | 5 |
| cli F2 | T1 | `ecp inspect` ambiguity hint says "Use --file or --kind", but those flags filter neighbours, so the agent loops. | `ecp inspect load --kind method --repo . --format json` returns `ambiguous Found 6 symbols matching 'load'. Use --file or --kind to disambiguate. 6`. Hint at `crates/ecp-cli/src/commands/inspect.rs:740`. | re-run | 5 and fix |
| cli F4 | T1 | `ecp impact --target load --kind method` still matches several symbols and merges them into one blast radius. | `ecp impact --target load --kind method --repo . --format json` returns four depth-0 rows from four unrelated files, status `success`. Check at `crates/ecp-cli/src/commands/impact/symbol.rs:153`. | re-run | 5 and fix |
| cli F5 | T1, T2 | Engines from `load_ensured` (find bm25, group, peers plan) pay the dirty-file parse but never attach the overlay. | `grep -rn "with_overlay" crates/ecp-cli/src` returns `main.rs:260` only; `crates/ecp-cli/src/engine.rs:116-126` sets `overlay_dir: None`. | code read | 5 |
| cli F6 | T1 | The stale caveat reaches 6 of 17 graph commands. `rename`, `pattern`, `routes`, `shape-check`, `tool-map`, `review`, `heuristics`, `processes`, `find-transaction-patterns`, `find-schema-bindings`, `find-event-mirrors` print no marker (the graph-backed set is the match at `crates/ecp-cli/src/cli.rs:179-194`, plus `group`). | `grep` of `.caveat()` finds callers only in find, inspect, cypher, path, impact, group (`crates/ecp-cli/src/engine.rs:144-152`). Same finding as freshness F5. | code read | 5 |
| cli F9 | T4 | A new command touches six enumerations. `ImpactArgs` has 23 fields, including one skipped internal field, and is also the internal API. | `crates/ecp-cli/src/main.rs:56,98,270-337,355-381`; `crates/ecp-cli/src/cli.rs:174,217`; `crates/ecp-cli/src/commands/impact/mod.rs:42-193`. | code read | 5 |
| cli F11 | T1 | A closure `owner_matches` in `inspect.rs` is linked to the function of the same name in `symbol_id.rs` at confidence 0.7. Module-alias calls drop out of the graph. | `ecp inspect owner_matches --repo .` lists `<anonymous>, search_nodes` at 0.7 as callers of `symbol_id.rs:57`. `ecp inspect --name run_for_symbol` misses the alias call at `crates/ecp-cli/src/commands/group/impact.rs:101`. | reviewer repro | 1 |
| cli F7 | T3 | `--format jsno` silently prints TOON. `--format text` on inspect prints pretty JSON. | `ecp find resolve_candidates --repo . --format jsno` prints TOON (`crates/ecp-cli/src/output.rs:29`). | re-run | fix |
| cli F10 | T3 | An ambiguous `inspect` returns 7,601 bytes where `impact` returns a 491-byte list. Constant keys such as `"processes": []` appear in every inspect. | `crates/ecp-cli/src/commands/inspect.rs:714-746,669,235-236`. | reviewer repro | fix |
| cli F12 | T1 | `std::process::exit(1)` in `find_schema_bindings` skips telemetry and the tantivy join in `main`. | `crates/ecp-cli/src/commands/find_schema_bindings.rs:229`; join at `crates/ecp-cli/src/main.rs:42-45`. Not reproduced. | code read | fix |
| cli Q3 | T3 | The same data has different field names across commands. `find` has no owner field, so `Foo.load` and `Bar.load` look alike. `factor_base_path` renames `filePath` to `relPath` only in the default format. | Default output uses `relPath`; `--format json` uses `filePath` (`crates/ecp-cli/src/output.rs:189-257`). `FindMatch` at `crates/ecp-cli/src/commands/find.rs:316-325`. | reviewer repro | none |
| cli F8 | T2 | `rename`, `pattern` and `schema-bindings` scan all nodes per call. `cypher` re-reads `registry.json` on each call with `--repo`. | `crates/ecp-cli/src/commands/rename.rs:314-345`; `crates/ecp-cli/src/commands/cypher.rs:72-81`. | code read | none |
| cli Q4-1 | T5 | `run_for_symbol` has nine parameters and ignores `timeout_ms`. | `crates/ecp-cli/src/commands/impact/symbol.rs:74-118,115`. | reviewer repro | none |
| cli Q4-2 | T5 | `impact::build_payload` is a dead pass-through (only tests call it). | `crates/ecp-cli/src/commands/impact/mod.rs:392-395`. | reviewer repro | none |
| cli Q4-3 | T5 | `Engine::open`, `Engine::view` and `GraphView` are dead in the binary. | `crates/ecp-cli/src/engine.rs:92,175,258`. | reviewer repro | none |
| cli Q4-4 | T5 | `graph_path::head_sha_bytes` is a one-line re-export. | `crates/ecp-cli/src/graph_path.rs:57-59`. | reviewer repro | none |
| cli Q4-5 | T5 | `heuristics::run` duplicates the dispatch of three deprecated verbs. | `crates/ecp-cli/src/commands/heuristics.rs:60-65`; `crates/ecp-cli/src/main.rs:306-314`. | code read | none |

### Types and session view

| id | tier | problem | evidence | status | goes to |
|---|---|---|---|---|---|
| types-session B1 | T1 | Overlay reads are opt-in per command. `rename`, `pattern` and 16 other commands answer from the committed graph with no marker. | `ecp rename beta_dirty beta_renamed --dry-run` prints `No occurrences of "beta_dirty" found.` `ecp pattern -p 'alpha()' --callers-of alpha` returns `scanned_files: 1`, `total: 2` (fresh.py missed). `ecp find beta_dirty` returns `total_candidates: 1`. | re-run | 5 |
| types-session B4 | T1 | Multi-repo find reads base-only engines, so a dirty sibling repo is invisible. Speculative; not verified. | `crates/ecp-cli/src/commands/find.rs:1424` calls `engine.graph()` inside the group `par_iter`. | code read | 5 |
| types-session A2 | T1, T4 | The language-to-resolver contract is a control-character string protocol. Each producer picks its marker by hand. No wrong output found; marker count per file ranges from 0 (c) to 10 (swift). | `crates/ecp-core/src/analyzer/types.rs:84-345`, markers at `:122-131`; consumers `crates/ecp-analyzer/src/resolution/builder.rs:2611` and `crates/ecp-core/src/session/view.rs:587`. | code read | 10 |
| types-session A3 | T4 | Fourteen copies of the extraction skeleton. One cross-language feature edits 13 files. | `git show --stat 9ff296a9` touched 13 of 14 `receiver_types.rs` files; `0f43423a` touched 13. Python copy diff prints `IDENTICAL_BODY`. | reviewer repro | 10 |
| types-session A5 | T1 | Kotlin enclosing-class ignores `Interface`, unlike Java and C#. A `this.m()` in a Kotlin interface body may bind nothing. Speculative; confirm first. | `crates/ecp-analyzer/src/kotlin/receiver_types.rs:477` against `java/receiver_types.rs:332` and `c_sharp/receiver_types.rs:390`. | reviewer repro | fix |
| types-session A4 | T5 | Python has a byte-identical second copy of `enclosing_symbol_and_owner`. | `crates/ecp-analyzer/src/python/receiver_types.rs:377` equals `python/path_literals.rs:153`. | reviewer repro | none |
| types-session A6 | T4 | The PHP entry returns a third tuple element, so it cannot share the entry signature. | `crates/ecp-analyzer/src/php/receiver_types.rs:108`. | code read | none |
| types-session B2 | T5 | `GraphView`, `Engine::open` and `Engine::view` are dead and carry `#[allow(dead_code)]`. | `crates/ecp-cli/src/engine.rs:86-95,175,258`. | reviewer repro | none |
| types-session B3 | T5 | The guard `engine.overlay_dir().and_then(\|_\| engine.overlay_view())` is redundant. | `crates/ecp-cli/src/commands/find.rs:586`; `crates/ecp-cli/src/commands/inspect.rs:597`. | code read | none |
| types-session Q6-1 | T5 | Twelve `enclosing_symbol_and_owner_pub` wrappers only widen visibility. | `crates/ecp-analyzer/src/c/path_literals.rs:99-104`. | reviewer repro | none |
| types-session Q6-2 | T5 | The Go wrapper `extract_go_calls_and_path_literals` has no caller. | `crates/ecp-analyzer/src/go/receiver_types.rs:454-461`. | reviewer repro | none |

### Flow

| id | tier | problem | evidence | status | goes to |
|---|---|---|---|---|---|
| flow F1 | T1 | `from ..util import f` in `pkg/a/sub/b.py` resolves to the sibling `pkg/a/sub/util.py` (a decoy) and reports 0 boundaries. The relative level is lost. | `replace('.', "/")` turns `..util` into `//util` (`crates/ecp-analyzer/src/flow/module.rs:21-40`). Raw output: `('return', 'pkg/a/sub/util.py', 2, 'return "DECOY"')`, `boundaries` was `[]`. | re-run | fix |
| flow F1b | T1 | Flow resolves relative imports only. Flow resolves Python module names relative to the importing file, without Python absolute-import search semantics, and JS/TS `./lib.js` for `lib.ts` fails. The boundary message blames the import form, not the path. | `ecp flow --file enoract/console/api/_helpers.py --line 65 --subject return --direction forward` returns nodes in 1 file. `ecp impact --target get_pool --direction upstream` returns 102 rows across files. | reviewer repro | 4 |
| flow F2 | T2 | Every flow query loads and evaluates the whole corpus. The cache has one slot per repo. | `--no-cache` on enoract (1077 files): 3.27 s, 3.13 s, 2.93 s, 3.01 s. Cache hit: 0.045 s. Budgets at 5,000,000 take 21.3 s for the same 152-node answer. | reviewer repro | measure |
| flow F3 | T1 | `truncated` is one corpus-wide latch. The `ast_budget` boundary of a cut file is filtered out and counted in `boundaries_omitted`. | With `--max-steps 150`: nodes only in `lib.py`, `consumers 1`, `truncated True`, `boundaries []`, `boundaries_omitted 22` (`crates/ecp-analyzer/src/flow/module.rs:226-235`). | reviewer repro | fix |
| flow F4 | T3 | Boundaries are 62% of the payload: 423 entries but 28 distinct messages. | JSON 117.5 KB, boundaries 72.6 KB; `ecp flow --format toon \| wc -c` gives 79215. | reviewer repro | none |
| flow F5 | T4 | Flow covers JavaScript, TypeScript, Python, and PHP. Each new language adds branches inside the 1451-line `engine.rs`. The extension list is a fourth copy. | `crates/ecp-analyzer/src/flow/mod.rs:111-125`; `crates/ecp-cli/src/commands/flow.rs:83-91`. | code read | none |
| flow F6 | T3 | Flow nodes carry no graph uid and no enclosing function name. | `crates/ecp-analyzer/src/flow/mod.rs:60-70`; `crates/ecp-cli/src/commands/flow.rs:166-180`. | code read | none |
| flow F7 | T1 | `shape_check --route X` counts routes before it filters. A matching route with no shape prints a false `No routes match`. `ecp review` maps `Err` to an empty list. | `crates/ecp-cli/src/commands/shape_check.rs:131-159,286`; `crates/ecp-cli/src/commands/review/aggregate.rs:355-358`. | code read | fix |
| flow F8 | T1 | The response shape comes from the whole file, so a multi-handler file compares against the union of its handlers. False "no drift". | `crates/ecp-analyzer/src/resolution/builder.rs:1812-1840`; `crates/ecp-analyzer/src/fetch_shape/response_shapes.rs:43-54`. | code read | fix |
| flow F9 | T2 | Flow source feeds `ECP_PARSER_FINGERPRINT`, so a flow edit may invalidate the graph cache. Not confirmed. | `crates/ecp-analyzer/build.rs:78-112`. | code read | none |

### Cypher

| id | tier | problem | evidence | status | goes to |
|---|---|---|---|---|---|
| cypher C1 | T1 | Comma-separated patterns in one MATCH are unioned, not joined. A test pins the defect. | `ecp cypher "MATCH (a:Function {name:'compute_hits'}), (b:Function {name:'compute_single'}) RETURN a.name, b.name"` returns `[["compute_hits",null],[null,"compute_single"]]`. `crates/ecp-core/tests/cypher_stream_aggregation.rs:38,76`. | re-run | 9 |
| cypher C2 | T1 | `EXISTS((a)-[:Calls]->(b))` ignores the binding of `b` when `a` is bound. | `MATCH (a:Function {name:'compute_hits'}) MATCH (b:Function) WHERE EXISTS((a)-[:Calls]->(b)) RETURN count(b)` returns `18616`; true count is `5`. `crates/ecp-core/src/cypher/executor.rs:2139-2167`. | re-run | 9 |
| cypher C3 | T1 | A second WHERE or WITH overwrites the first. MATCH after WITH is hoisted. ORDER BY and LIMIT bind to the right UNION branch only. | `MATCH (a:Function) WITH a WHERE a.name = 'compute_hits' WITH a RETURN count(a)` returns `18616`, expected `1`. `crates/ecp-core/src/cypher/parser.rs:110-129`. | re-run | 9 |
| cypher C4 | T1 | `count(DISTINCT var)` and `collect(DISTINCT var)` dedup nodes by name. | `MATCH (f:Function) RETURN count(DISTINCT f) AS byvar, count(DISTINCT f.uid) AS byuid, count(f) AS c, count(DISTINCT f.name) AS byname` returns `[15718, 18614, 18616, 15718]`. | re-run | 9 |
| cypher C5 | T1 | ORDER BY on a non-projected expression is a silent no-op, so LIMIT returns arbitrary rows. | `MATCH (a:Function) RETURN a.name AS n ORDER BY a.name DESC LIMIT 3` returns node order, not descending. `executor.rs:634-669`. | reviewer repro | 9 |
| cypher C6 | T1 | The lexer corrupts non-ASCII bytes and drops the backslash of `\d`. | `n.name =~ "^[a-z_]+\d$"` returns `1333`; with `[0-9]` it returns `42`. `n.name CONTAINS '→'` returns `0`. `crates/ecp-core/src/cypher/lexer.rs:224-236`. | re-run | 9 |
| cypher C7 | T1 | Unknown scalar functions, unbound variables, unknown labels in WHERE and type mismatches all return Null or 0 with no error. | `RETURN coalesce(n.ownerClass,'x'), toUpper(n.name), size(n.name)` returns `[null,null,null]`. `WHERE b.name = 'x'` with `b` unbound returns `0`. `executor.rs:1070-1117`. | reviewer repro | 9 |
| cypher C8 | T1 | Variable-length `*m..n` returns node reachability by shortest depth, not paths. | Distinct pairs for `-[:Calls]->(m)-[:Calls]->` is 22950; for `-[:Calls*2..2]->` it is 22246. `executor.rs:1746-1776`. | reviewer repro | 9 |
| cypher C9 | T1 | A WHERE after OPTIONAL MATCH filters the whole result, not the optional match. | `MATCH (f:Function {name:'compute_hits'}) OPTIONAL MATCH (c)-[:Calls]->(f) WHERE c.name = 'zzz_nonexistent' RETURN f.name, count(c) AS n` returns no rows; expected `[compute_hits, 0]`. | reviewer repro | 9 |
| cypher C10 | T1 | `.content` reads from the CLI working directory, not the repo root. A subdirectory run returns an empty string. | After `cd crates/ecp-cli`, `RETURN n.filePath, n.content` returns `["crates/ecp-cli/src/commands/graph_csr.rs",""]`. `crates/ecp-cli/src/commands/cypher.rs:65-70`; `executor.rs:81-93`. | reviewer repro | 9 |
| cypher C11 | T1 | Metadata properties return fabricated defaults for nodes without FunctionMeta. Every Struct and Property reads as public. | `MATCH (n:Struct) RETURN n.visibility, count(*)` returns `[[0,635]]`. `executor.rs:2543-2545`. Overlay part not run. | reviewer repro | 9 |
| cypher C12 | T1 | Bare `RETURN var` after OPTIONAL MATCH depends on the first row. | `MATCH (f:Function) OPTIONAL MATCH (c)-[:Calls]->(f) RETURN c LIMIT 2` fails with `semantic error: unbound variable 'c'`. `executor.rs:770-843`. | reviewer repro | 9 |
| cypher C13 | T2 | Non-aggregate queries materialise every binding, and LIMIT applies last. | `MATCH (a)-[r]->(b) RETURN a.name, b.name LIMIT 3` takes 0.05-0.10 s and 160 MB; the `count(*)` form takes 0.02-0.03 s and 26 MB. | reviewer repro | 9 |
| cypher C14 | T2 | Fast seed paths turn off whenever a session overlay exists. Not measured. | `crates/ecp-core/src/cypher/executor.rs:1365,1433-1435`. | code read | 9 |
| cypher C15 | T4 | A new node property needs five hand-synced sites. No test ties them together. | `executor.rs:1827,2482`; `crates/ecp-core/src/cypher/diagnostics.rs:17`; `crates/ecp-cli/src/commands/cypher.rs:18-21`. | code read | 9 |
| cypher C16 | T1 | `graph_csr` is a base-only incoming-edge walk. `find` caller counts can disagree with cypher on a dirty tree. Not run. | `crates/ecp-cli/src/commands/graph_csr.rs:22-58`; `crates/ecp-cli/src/commands/find.rs:467-472,552`. | code read | 9 |
| cypher C17 | T3 | Parse errors print a token index as a byte offset and place the caret there. | `... WHERE n.name == 'x' ...` prints "parse error at byte 12"; the `==` is at column 36. `parser.rs:50-57`. | reviewer repro | 9 |
| cypher C18 | T1 | The output never says the answer includes uncommitted edits. Speculative. | `crates/ecp-cli/src/commands/cypher.rs:101-124`; `crates/ecp-cli/src/engine.rs:144-163,216-240`. | code read | none |

### Agent integration (hooks, MCP, skill)

| id | tier | problem | evidence | status | goes to |
|---|---|---|---|---|---|
| agent A1 | T1 | MCP and native spawn return stdout only on success. The text-mode stale caveat goes to stderr, so a stale `no match` reads as "does not exist". | `crates/ecp-mcp/src/spawn.rs:15-33`; `eprintln!("note: {c}")` at `crates/ecp-cli/src/commands/find.rs:662`. | re-run | 5 |
| agent A2 | T1 | The PreToolUse hook picks its graph by mtime with no distance gate, no caveat and no overlay. | `crates/ecp-cli/src/commands/hook/common.rs:141` against `WARM_ATTACH_MAX_DISTANCE = 1` at `crates/ecp-cli/src/auto_ensure.rs:869`. `grep -n 'caveat\|stale\|behind' pre_tool_use.rs` returns nothing. | code read | 5 |
| agent A3 | T1, T3 | The MCP tool list (22 tools) includes `ecp_uninstall` and `ecp_usage`. `uninstall` with no flags wipes `~/.ecp` and the binary. | `ecp admin mcp tools --format json` lists `ecp_uninstall` (776 B) and `ecp_usage` (1,308 B). `crates/ecp-mcp/src/schema.rs:72-95`; `crates/ecp-cli/src/commands/uninstall.rs:93-115`. | re-run | fix |
| agent A4 | T1, T4 | The `ecp summary` framework catalog is a hand copy. On a Laravel, Gin, Rails or ASP.NET repo, `frameworks.detected` is empty. | `ecp summary --repo .` prints `supported_count: 15`. `crates/ecp-cli/src/commands/summary.rs:342-440`; `crates/ecp-analyzer/src/framework_confidence.rs:9-44`. | code read | 8 |
| agent A5 | T1, T2 | `tool-map` reads the working tree as text and covers five languages through four dispatch branches. Nine of 14 mainstream languages return `{"status":"success",...}` with no marker. `review` swallows the error. | `crates/ecp-cli/src/commands/tool_map.rs:293-317`; `crates/ecp-cli/src/commands/review/aggregate.rs:254`. | code read | none |
| agent A6 | T2 | The hook builds 100 hits and shows 5. The value-flow comparison runs in both PreToolUse and PostToolUse. | Grep hook 17-18 ms against 9 ms for a non-search hook. `crates/ecp-cli/src/commands/find.rs:54,1100,1113-1156`; `crates/ecp-cli/src/commands/hook/pre_tool_use.rs:13-14,28`. | reviewer repro | fix |
| agent A7 | T5 | The git reference-transaction hook calls `rename-branch` and `prune`, which do not exist. | `ecp rename-branch --help` prints `unrecognized subcommand 'rename-branch'`. `crates/ecp-cli/src/commands/hook_watcher.rs:55-60`. | reviewer repro | fix |
| agent A8 | T3 | The update notice enters the model context with an instruction (`Run ecp update`) the model must not follow. | `crates/ecp-cli/src/commands/hook/user_prompt_submit.rs` (`drain_update_notice`); `crates/ecp-mcp/src/schema.rs:87-89`. | code read | fix |
| agent A9 | T5 | Telemetry and usage dashboards serve humans only. One MCP call is recorded twice, as `ecp_find` and `find`. | `crates/ecp-cli/src/main.rs:42`; `crates/ecp-mcp/src/server.rs:58-69`; `crates/ecp-cli/src/commands/usage.rs:145-149`. | code read | filter |
| agent A10 | T4, T5 | Five host installers are stubs shown as working. The Codex native scaffold is dead. | `crates/ecp-cli/src/admin/host_integration/mcp/cursor.rs:5-14` prints "coming in subproject C."; `native/codex.rs::run_install` always returns Err. | code read | fix |
| agent T5-1 | T5 | `hint.rs` has three of four helpers unused behind `#![allow(dead_code)]`. | `crates/ecp-cli/src/hint.rs`. | code read | none |
| agent T5-2 | T5 | `insight.rs` (242 lines, hidden) duplicates `ecp usage --source mcp`. | `crates/ecp-cli/src/commands/insight.rs`. | code read | none |

### Frameworks and routes

| id | tier | problem | evidence | status | goes to |
|---|---|---|---|---|---|
| frameworks F1 | T1 | The HTTP verb comes from a substring of the whole decorator, at confidence 1.0. `@PostMapping("/get-token")` becomes `GET /get-token`. | `ecp cypher 'MATCH (h)-[e:HandlesRoute]->(r:Route) RETURN h.name,r.name,e.confidence,e.reason'` returns `token,GET /get-token,1,decorator`. `crates/ecp-analyzer/src/route_detector.rs:63-74`. | re-run | 8 and fix |
| frameworks F2 | T1 | Three route producers are never reconciled. NestJS yields two Route nodes with one uid. Gin and JS Express handlers print `inline_anonymous`. A test fixture becomes an EntryPoint. | `ecp routes --format json` returns two rows with `"uid":"13927153653371215464"` (lines 6 and 7). `src/test/java/FakeCtlTest.java` gives `EntryPoint route@fx -> fx`. `crates/ecp-analyzer/src/resolution/builder.rs:1393-1400,2049-2056`. | reviewer repro | 8 |
| frameworks F3 | T1 | Only PHP and Ruby compose route prefixes. FastAPI, Gin, Express, NestJS and Spring report the unprefixed path. Ktor and Axum yield no Route. | FastAPI `APIRouter(prefix="/api/v2")` with `@router.get("/items")` gives `GET /items`. | reviewer repro | 8 |
| frameworks F4 | T1 | `ecp contracts` is a stub that returns an empty result as success, and the shipped skill tells agents to use it. | `ecp contracts --repo "<probe2>,<probe4>"` printed `pairs: []`, `repos_scanned: 2`. `crates/ecp-cli/src/commands/contracts.rs:246-251`. Same finding as freshness F8. | re-run | 8 |
| frameworks F5 | T1, T4, T2 | Route response shape and consumer keys are file-wide. The `Fetches` source is the first node in the file. Consumer coverage is TS, JS and Python only. | `ecp shape-check` returns `drift: [], drift_count: 0`, but `Fetches` carries `keys:beta`. `Fetches` source printed `unrelatedHelper,1,GET /a`. `builder.rs:1787-1975`. | reviewer repro | 7 |
| frameworks F6 | T4 | File-level hints for C#, Swift, Ruby, C++ and Dart attach to `<module>` and are dropped in pass 2. | `ecp cypher 'MATCH (a)-[e:References]->(b) ...'` returned one edge; no `aspnet-attribute`, `vapor-routing` or `flutter-*` reason. `builder.rs:2783-2786`; `framework_helpers.rs:857`. | reviewer repro | 8 |
| frameworks F7 | T4, T1 | Five detector shapes across the languages. The framework catalog exists in four hand-kept copies, and `ecp summary` under-reports. | `ecp summary --detailed` printed `supported_count: 15` and `detected: Java/Spring, TypeScript/NestJS`. `crates/ecp-cli/src/commands/summary.rs:345-424`. | reviewer repro | 8 |
| frameworks F8 | T1 | Event topics are stored canonicalised, so the node name is not a string in the source. | `producer.send("prod.order-created.v2", ...)` gives `ecp cypher 'MATCH (n:EventTopic) RETURN n.name,n.filePath'` -> `order/created`. `crates/ecp-analyzer/src/event_topic/extract.rs:111-118`. | reviewer repro | 8 |
| frameworks F9 | T3 | `ecp heuristics event-mirrors` emits constant fields per row and accepts a `--lib` flag that does nothing. | `crates/ecp-cli/src/commands/find_event_mirrors.rs:280,297-308,337-348`. | code read | 8 |
| frameworks F10 | T2 | `ecp heuristics saga` scans the whole graph several times per query. Speculative; not timed. | `crates/ecp-cli/src/commands/find_tx_patterns.rs:102,230,271,323`. | code read | none |

### Freshness and incremental

| id | tier | problem | evidence | status | goes to |
|---|---|---|---|---|---|
| freshness F1 | T1 | From a subdirectory, uncommitted edits are invisible. Porcelain paths are joined onto the working directory, not the repo top. | `cd pkg/sub && ecp find bar --repo .` returns `found: false`; from the root it returns `found: true`, `pkg/sub/a.py:3`. `crates/ecp-cli/src/auto_ensure.rs:445-448,504-515`. | re-run | 5 |
| freshness F2 | T1 | Deleted and renamed files stay in the answer. No tombstone is written. | After `rm other/b.py`, `ecp find baz --repo .` returns `found: true ... other/b.py,Function,1,baz`. `crates/ecp-cli/src/reanalyze.rs:144,198-209`. | re-run | 5 |
| freshness F3 | T1, T4 | The overlay parses 19 languages and the cold index parses 37. Same finding as dsl F1 and index F2. | `run.sh` edited from `old_fn` to `new_fn`: `ecp find new_fn` returns `found: false`; `ecp find old_fn` returns `found: true run.sh:1`. `crates/ecp-cli/src/reanalyze.rs:34-54`. | re-run | 6 |
| freshness F4 | T1, T4 | The shadow-candidate cascade writes fragments with mtime 0, and the reader rejects them. Overlay and index bind one call to different targets. | `dirty_files.json` holds `lib/bar.js 0`. `ecp impact --target helper --direction upstream --file lib/bar.ts` returns 0 callers. `crates/ecp-cli/src/auto_ensure.rs:1224-1232`. | reviewer repro | 11 |
| freshness F5 | T1 | The stale caveat has callers in six command families; coverage varies by execution path. Same finding as cli F6. | `ecp routes` returns `results: []` with no caveat. `ecp rename zed zed2 --dry-run` prints `No occurrences of "zed" found.` `crates/ecp-cli/src/engine.rs:144-163`. | reviewer repro | 5 |
| freshness F6 | T2 | Per-query cost grows with the count of dirty files that no provider can parse. | 10k-file repo: 15 ms clean, 88 ms with 12,000 untracked `.csv` files. The vscode corpus: 162 ms per query against 18 ms graph load. | reviewer repro | fix |
| freshness F7 | T2 | Without a host session id each query is a new session: a full re-parse and a new directory. | With `env -u CLAUDE_CODE_SESSION_ID -u ECP_SESSION_ID`, `l1.refreshed written=1` appears in all three runs. The resolver also reads `GEMINI_CLI_SESSION_ID`, `CODEX_SESSION_ID` and `CODEX_THREAD_ID`; a full reproduction clears all five variables. `crates/ecp-cli/src/session/resolver.rs:18-31`. | reviewer repro | measure |
| freshness F8 | T1 | `ecp contracts` is a stub that answers "no contracts" with success. Same finding as frameworks F4. | `ecp contracts --repo @all` over 178 repos returns `pairs: []`, `unmatched_producer_count: 0`. `crates/ecp-cli/src/commands/contracts.rs:246-251`. | re-run | 8 |
| freshness F9 | T5 | `incremental.rs` (662 lines), `with_symbol_skip_set`, and test-only writer routes are dead in production. | `ecp impact --target symbol_hash_diff --direction upstream` lists only tests. `crates/ecp-analyzer/src/resolution/builder.rs:211,225`. | reviewer repro | filter |
| freshness F10 | T1 | A failed background rebuild leaves no marker, so every query respawns it. Speculative. | `crates/ecp-cli/src/auto_ensure.rs:928-952`; `crates/ecp-cli/src/commands/hook/post_tool_use.rs:106-126`. | code read | fix |
| freshness F11 | T5 | Admin TUI menus, the config wizard and self-update serve humans. | `crates/ecp-cli/src/admin/` (about 835 lines); `crates/ecp-cli/src/commands/update.rs` (1964 lines). | code read | filter |
| freshness parity gap | T1 | `overlay_differential_parity` covers outgoing `Calls` edges only. Separate tests compare `References` edges (`overlay_variant_ref_parity.rs`). A comprehensive node-and-edge matrix, incoming edges, deletions and renames remain unestablished. | `crates/ecp-cli/tests/overlay_differential_parity.rs:1-5` compares only `Calls` edges out of a dirty file; `KNOWN_DIVERGENCES = []`. | code read | 11 |

### Index pipeline

| id | tier | problem | evidence | status | goes to |
|---|---|---|---|---|---|
| index F1 | T1 | One commit slot gets two source trees. A clean tree is walked in place; a dirty tree goes through `git archive`. The two apply different ignore policies, so the same sha can yield different graphs. | Walk of the repo: `src/a.py`, `tests/t.py` (`src/ig.py` skipped). Walk of `git archive HEAD \| tar -x`: `src/a.py`, `src/ig.py` (`tests/t.py` gone). `crates/ecp-cli/src/build/orchestrator.rs:139-146`. Reproduced with git and ripgrep, not ecp. | code read | 3 |
| index F2 | T1 | The overlay pipeline has 19 of 37 providers. Same finding as dsl F1 and freshness F3. | `crates/ecp-cli/src/reanalyze.rs:34-54` against `provider_registry.rs` (`PROVIDER_CONSTRUCTORS`, 37 entries). | re-run | 6 |
| index F3 | T1 | When tantivy is missing, `find --mode bm25` falls back to substring scan for the commit's lifetime and prints no caveat. The two rankers return different sets. | For the fragment `onfig`, substring gives `parseConfig` (score 0.4); tantivy returns an empty list. `crates/ecp-cli/src/commands/find.rs:1066-1083,1127-1132`; `crates/ecp-cli/src/build/orchestrator.rs:329-361`. | code read | 5 |
| index F4 | T4, T5 | Extension-to-provider knowledge lives in five distinct mappings. The markdown and yaml providers are dead end to end. | `MATCH (f:File) WHERE f.filePath ENDS WITH ".md" RETURN count(f)` returns 0, and `MATCH (f:Document) RETURN count(f)` returns 0. `crates/ecp-core/src/analyzer/pipeline.rs:84-194`; `crates/ecp-cli/src/commands/admin/index.rs:569-630`. | reviewer repro | 6 |
| index F5 | T1 | `Language` has no Vue, Svelte, Astro or Protobuf variant. A `.vue` file gets `Language::Unknown`, so Tier-3 resolution cannot cross to `.ts` targets. Same finding as core F4. | `crates/ecp-core/src/file_category.rs:261-322,400-402`; `crates/ecp-analyzer/src/resolution/builder.rs:1076`. Code only; no Nuxt fixture was run. | code read | 6 |
| index F6 | T1 | Two file-size caps have different sources and defaults: 1 MiB in the walk, 16 MiB in the pipeline. | `crates/ecp-core/src/config.rs:174-184`; `crates/ecp-core/src/analyzer/pipeline.rs:15-20,253-268`. | code read | 6 |
| index F7 | T1 | The `Fetches` edge source is the first node in the file, not the caller. Same finding as frameworks F5. Pass 1.6 re-reads source from disk. | `crates/ecp-analyzer/src/resolution/builder.rs:1864-1883,1914-1915`. The repo graph has no Fetches edges (`MATCH ()-[r:Fetches]->() RETURN count(r)` = 0). | code read | 7 |
| index F8 | T2 | Pass 1 allocates a `String` path per node. Pass 2 builds a fresh `Resolver` per file. Speculative. | `crates/ecp-analyzer/src/resolution/index.rs:473,628`; `builder.rs:3081`. | code read | measure |
| index F9 | T2 | Every CLI process re-validates the whole graph with a full `rkyv::access` walk. | `crates/ecp-cli/src/engine.rs:24-26` says 2-4 ms on an 88 MB graph; `orchestrator.rs:270-273` says 10-50 ms. `README.md:77` reports `ecp find` on vscode at 34.4 ms. | code read | measure |
| index F10 | T2 | A tantivy hit maps back to the graph by an O(N) node scan. Speculative; no timing. | `crates/ecp-cli/src/search.rs:141-145,172`; `crates/ecp-cli/src/commands/find.rs:1085-1106`. | code read | measure |
| index F11 | T1 | `ecp admin index` reports `l2.exists` after a binary upgrade and skips the rebuild that `build_l2` would do. | `crates/ecp-cli/src/commands/admin/index.rs:376-394,481-495` against `orchestrator.rs:81,329-361`. | code read | 3 and fix |
| index F12 | T5 | Leiden community detection serves only `ecp processes`. It cost 0.19 s of 3.13 s in the May table. | `crates/ecp-analyzer/src/resolution/builder.rs:456-467`; `crates/ecp-cli/src/commands/processes.rs:113-143`. | code read | filter |
| index T5-1 | T5 | `native.rs` (208 lines) has no caller outside `lib.rs`. | `crates/ecp-cli/src/native.rs:9`. | code read | none |
| index T5-2 | T5 | `build/mode.rs` (34 lines) is dead; only a test uses it. | `crates/ecp-cli/src/build/mode.rs`. | code read | none |
| index T5-3 | T5 | Pass 2 has a serial dump body and a parallel production body that are near copies. | `crates/ecp-analyzer/src/resolution/builder.rs:2984-3175`. | code read | none |
| index T5-4 | T5 | The `__brick__` filter reads the absolute path, so a checkout under such a directory indexes nothing. Confidence 50. | `crates/ecp-cli/src/commands/admin/index.rs:545-554`. | code read | none |

### Analyzer post-parse passes

| id | tier | problem | evidence | status | goes to |
|---|---|---|---|---|---|
| postparse F1 | T1 | Post-process passes find the enclosing symbol by (file, name). The edge lands on the first same-named symbol, such as a trait declaration instead of the impl. | `ecp cypher 'MATCH (f)-[r:UsesPathLiteral]->(p:PathLiteral) WHERE ... f.ownerClass <> p.ownerClass ...'` gives 22 rows; 6 are wrong-source, such as `read_cached_impact,GhClient,RealGhClient,430,541`. `crates/ecp-analyzer/src/post_process/path_literal_nodes.rs:77`. | re-run | 7 |
| postparse F1 owner spelling | T1 | Two owner computations disagree. 16 rows show the owner as `Walk<'s, 't>` on the literal and `Walk` on the function. | Same query as postparse F1: `Walk<'s, 't>`, `Resolver<'a>`, `OverlayFacts<'a>`, `RustModules<'a>` against `Walk` and similar. | re-run | 2 |
| postparse F2 | T1 | `Overrides` edges match by class name and method name only, at confidence 1.0. The marker gate covers 4 of 14 languages. The doc promises an arity check. | `crates/ecp-analyzer/src/post_process/overrides.rs:16-18,99-105,156-157`; `crates/ecp-core/src/graph.rs:147-152`. Swift `override func viewDidLoad()` yields no edge. | code read | 7 |
| postparse F3 | T1, T4 | Indirect-dispatch flags come from file-wide name sets. Only `ecp diff` and `ecp review` read them. In Python, `def f(user): user.save()` yields a false WARN. | `crates/ecp-analyzer/src/indirect_dispatch.rs:630-660`; `crates/ecp-cli/src/commands/diff/symbols.rs:292-330`. | code read | 7 |
| postparse F4 | T1 | `is_test` comes only from function_meta, so 12 languages have none. Python and JS also infer it from the function name. | Parsers set `raw_function_metas: vec![]` (`lua/parser.rs:316`, `zig/parser.rs:292`, `solidity/parser.rs:214`). `crates/ecp-cli/src/commands/impact/coverage.rs:39-48,84-97`. | code read | 10 |
| postparse F5 | T2 | The language query re-runs for pre-scans and extractors. Python walks the tree eight more times. TypeScript runs the schema query on every `.ts` file. | `crates/ecp-analyzer/src/python/parser.rs:744,799,1487,1499`; `typescript/parser.rs:341,897,907`. Not timed. | code read | 10 and measure |
| postparse F6 | T2 | Thirteen call extractors attach each call to its function by an O(nodes) scan. Not measured. | `crates/ecp-analyzer/src/calls.rs:156-203` against the sweep at `:1329-1393`. | code read | 10 and measure |
| postparse F7 | T1 | `ecp rename` stage 2 matches identifier text, so `Foo.validate` and `Bar.validate` in one file are both rewritten. A `.vue`, `.svelte` or `.astro` caller is skipped with no notice. The dry run prints a constant `risk safe`. | `crates/ecp-analyzer/src/identifier_finder/mod.rs:87` (`_ => Vec::new()`); `crates/ecp-cli/src/commands/rename.rs:411,446`. Not run. | code read | 5 and 6 |
| postparse F8 | T1 | Steps 3c-3f of file `Imports` edges guess a basename or directory match, yet all emit confidence 0.9. A third Rust module-path implementation sits beside `rust_paths.rs`. No wrong edge observed. | `crates/ecp-analyzer/src/post_process/imports_edges.rs:238-397`. `ecp cypher` over `post_process:imports:module` gives 425 edges, 0 cross-crate. | code read | 4 |
| postparse F9 | T4 | Adding a language touches about nine files in this scope. Coverage is uneven: variant refs 11/14, local-types walk 7/14, indirect dispatch 6/14, Overrides 4/14. | `crates/ecp-cli/src/commands/schema.rs:58-180`; `crates/ecp-core/src/analyzer/pipeline.rs:91`. | code read | none |
| postparse F10 | T5 | `post_process/event_topics.rs` (125 lines) and `ast_test.rs` (34 lines) are orphans. The `typescript.rs` mock provider sits in non-test code. `build.rs` omits six files from the fingerprint. | `crates/ecp-analyzer/src/post_process/mod.rs`; `crates/ecp-analyzer/build.rs:20-26`. | code read | filter |
| postparse M-1 | T1 | The innermost-owner rule exists in three copies that disagree on whether Trait counts as an owner. | `class_membership.rs:234-241`; `overrides.rs:137-141,224-233`; `calls.rs:186-198`. | code read | 7 |
| postparse M-2 | T1 | `saga_pairs` keys by simple owner name, so same-named classes share a bucket. The edge is tagged heuristic (0.6/0.8). | `crates/ecp-analyzer/src/post_process/saga_pairs.rs:108-148`. | code read | 7 |
| postparse M-3 | T5 | `scope_defines` calls `Language::from_normalized_path` for every raw node with an owner. | `crates/ecp-analyzer/src/post_process/scope_defines.rs:97-105`. | code read | none |
| postparse M-4 | T5 | `is_sql_shaped` allocates two Strings per string literal. Unmeasured. | `crates/ecp-analyzer/src/sql_literal/mod.rs:20-33`. | code read | none |
| postparse M-5 | T3 | A path with no registered provider returns silently, while other give-ups report a blind spot. Also an `mmap` SIGBUS risk if a file shrinks (not reproduced). | `crates/ecp-core/src/analyzer/pipeline.rs:243-245,292-296`. | code read | 6 |
| postparse M-6 | T5 | Recursive walkers have no depth bound on rayon workers. Not reproduced. | `crates/ecp-analyzer/src/indirect_dispatch.rs:139,273,441,577`. | code read | none |

### parse_file bodies

| id | tier | problem | evidence | status | goes to |
|---|---|---|---|---|---|
| parsefile D | T4 | 40.0% of the 7,899 body lines in the 14 `parse_file` functions is the same logic in three or more languages. 33.2% is script-corroborated. | The labelled lines and the recount script are not in the repo; treat the shares as a reported measurement. `python3 -I recount.py labels.csv` prints `TOTAL 7899 3156 40.0% 680 8.6% 2815 35.6% 1248 15.8%` (D, S, L, B). Top concept: `raw_node_construct`, 793 lines in 43 blocks. | re-run | 10 |
| parsefile helpers | T4 | Helpers exist but languages reimplement them. `body_has_call` has 12 private copies. The `thread_local PARSER` pool is redeclared in 13 files. `RawNode` has no constructor, so 44 hand-written literals exist. | `ecp find body_has_call --repo .` prints `total_candidates: 12`. Seven copies share md5 `e787c9` after normalisation. `grep -rn "RawNode::" crates/ecp-analyzer/src` (non-test) prints `0`. | re-run | 10 |
| parsefile commits | T4 | Fan-out commits edit 14 parsers at once. 52 commits touch five or more `parser.rs` files; 47 are substantive. | `git log` script prints: 52 commits touch >=5 parsers; 26 touch all 14. `git show --numstat` for `9ff296a9`: 14 files +540 -66. | re-run | 10 |

### Core graph and registry

| id | tier | problem | evidence | status | goes to |
|---|---|---|---|---|---|
| core F1 | T1 | A second same-identity declaration of a non-method kind (cfg twins, conditional redefinitions) becomes a tombstone with an empty name. `ecp find` shows only one copy. | `ecp cypher "MATCH (n:Function) WHERE n.name='' RETURN n.filePath, n.startLine" --repo .` returns 15 rows. `ecp find with_windows_retry` returns only `crates/ecp-core/src/registry/fs_safe.rs:21`; the twin at `:43` is hidden. `crates/ecp-core/src/uid.rs:30-36`. | re-run | 2 |
| core F2 | T3 | The uid-collision hint prints raw U+001F separators and no line number. | `ecp inspect --name with_windows_retry --repo .` prints `uid-collision: first=Function\u001fcrates/ecp-core/src/registry/fs_safe.rs\u001f\u001fwith_windows_retry second=...`. `crates/ecp-core/src/graph.rs:967-1001`. | re-run | fix |
| core F3 | T1 | Process traces start from a callees-over-callers score and include 0.5-0.7 edges. One false 0.7 edge links three subsystems. `EntryPoint` nodes are unused. | `ecp processes trace "Parse_file → Starts_with" --repo .` steps through `swift/parser.rs`, `session/view.rs`, `rust_module_path_base`, `starts_with`. `MATCH (a {name:'rust_module_path_base'})-[r:Calls]->(b {name:'starts_with'}) ...` returns `rust_paths.rs, 0.7, call`. | reviewer repro | none |
| core F4 | T1, T4 | Two extension tables disagree. `Language` maps `vue`, `astro`, `svelte`, `proto` and `json` to `Unknown`, so Tier-3 resolution cannot pick `.ts` targets. Same finding as index F5. | `crates/ecp-core/src/file_category.rs:261-320,400` against `crates/ecp-core/src/analyzer/pipeline.rs:91-143`. Code only; no end-to-end repro. | code read | 6 |
| core F6 | T3 | Error texts name `ecp analyze` and `--uid`, which do not exist. | `ecp analyze` returns `unrecognized subcommand 'analyze'`. `crates/ecp-core/src/error.rs:12,17`; `crates/ecp-cli/src/engine.rs:338,345`. | reviewer repro | fix |
| core F8 | T1 | The overload mark is derived by recomputing the uid hash at query time. A missed schema bump would make every Method read as a later overload. Latent. | `crates/ecp-core/src/graph.rs:1440-1457`; `crates/ecp-core/src/registry/commit_meta.rs:76`. | code read | none |
| core F9 | T1 | The `heuristic` column of `ecp schema reltypes` is a hand copy of `RelType::is_heuristic`. They agree today. Latent. | `crates/ecp-cli/src/commands/schema.rs:284-307` against `crates/ecp-core/src/graph.rs:147`. | code read | fix |
| core F5 | T5 | `graph_query.rs` has no production callers except `build_uid_index`. Its module doc claims `impact` shares it. | `ecp impact` showed 0 callers; grep confirmed. `crates/ecp-core/src/graph_query.rs`; `crates/ecp-cli/src/commands/impact/coverage.rs:169`. | reviewer repro | filter |
| core F11 | T5 | `algorithms/louvain.rs` (344 lines) has no importer. | grep finds only doc mentions in `leiden.rs` and `mod.rs`. | reviewer repro | filter |
| core F7 | T5 | `output.default_format` in config has no reader. A doc comment is stale. | `crates/ecp-core/src/config.rs:46-51,64-66`. | code read | none |
| core F10 | T1 | Leiden clamps community ids above 65535. `processes --community` truncates with `as u16`. Kinds excluded by `is_symbol` keep community 0. `Class` and `Interface` nodes take part in clustering. Minor. | `crates/ecp-core/src/algorithms/leiden.rs:63,171-176`; `crates/ecp-cli/src/commands/processes.rs:113`. | code read | none |
| core F12 | T5 | A corrupt `registry.json` returns an error, and the recovery path runs only on a version mismatch. | `crates/ecp-core/src/registry/store.rs:153-168`; `registry/mod.rs:41-44`. | code read | none |
| core F13 | T5 | `pool.rs` casts offsets with `as u32` and has no overflow check. Speculative. | `crates/ecp-core/src/pool.rs:30-39`. | code read | none |
| core F14 | T5 | `nodes_by_kind` keeps a linear-scan fallback for a graph version the loader rejects. | `crates/ecp-core/src/graph.rs:1257-1273`. | code read | none |

### Tests

| id | tier | problem | evidence | status | goes to |
|---|---|---|---|---|---|
| tests T-1 | T1 | The largest overlay parity file (143 tests) keys edges by name only, with no kind and no line. It sorts and dedups, so a doubled or wrong-line overlay edge passes. The default wrapper supplies no aliases or repo root; the configurable harness supports caller-supplied builders and already tests aliases. | `crates/ecp-cli/tests/overlay_resolver_parity.rs:208-230` (`targets()`, key `(file, name, owner_class, round(confidence*100))`); `crates/ecp-cli/tests/common/parity.rs:160` uses `GraphBuilder::new`; production at `crates/ecp-cli/src/commands/admin/index.rs:299-302`. | re-run | 11 |
| tests T-2 | T1 | Stored-data fixtures only assert rejection. The newest graph fixture is format v13; the code is at v15. Each overlay fixture has one file. | `graph_written_by_0_15_0`, `header_compatible_rejects_graph_written_by_0_15_0` and `ensure_index_returns_stale_for_graph_written_by_0_15_0` in `crates/ecp-cli/tests/version_mismatch_triggers_reindex.rs`; `GRAPH_FORMAT_VERSION` is 15 (`crates/ecp-core/src/graph.rs:36`); `BUILDER_FINGERPRINT` is `+schema20` (`crates/ecp-core/src/registry/commit_meta.rs:76`). | code read | 11 |
| tests T-3 | T4 | Fixture and helper setup is copy-pasted. 227 of 415 analyzer test files define their own parse helper (about 130 are `fn parse(src) -> LocalGraph`). 102 CLI files read `CARGO_BIN_EXE_ecp` directly. | `RawNode` has no `Default` (`crates/ecp-core/src/analyzer/types.rs:409-411`); 24 literals in 16 test files. `GraphFixture` is the healthy model. | code read | 10 |
| tests T-4 | T4 | The 14-language matrix has holes and no mechanical gate. `type_annotations`, `method_classification`, `local_var_filter` and `constructor` have 5 languages; `owner_collision` and `enum` have 3. | `scripts/parity/lang_matrix_audit.py` audits node kinds, not test coverage. Holes are by file name; a combined file may cover a cell. | code read | 11 |
| tests T-5 | T1 | No test checks that `provider_name_for_path` and `should_analyze_path` agree. A missing extension in one produces zero nodes while `parse_file` tests stay green. | Direct tests cover three `should_analyze_path` path cases (`crates/ecp-cli/src/commands/admin/index.rs:658-675`); they do not exhaustively compare it with `provider_name_for_path`; `provider_name_for_path` is asserted for six paths at `overlay_writer.rs:222-233`. No mismatch was run. | code read | 6 |
| tests T-6 | T5 | Stale parity baselines and tooling. CI clones 1.8 GB into `.sample_repo` for one test that passes when the directory is absent. | `.github/workflows/ci.yml:283-292`; `crates/ecp-analyzer/tests/c_cpp_macro_fallback.rs:21-79`. Eight baselines have no referrer. | code read | 11 |
| tests T-7 | T5 | 37 debug binaries in `ecp-analyzer/src/bin/test_*.rs` (530 lines) are compiled by `cargo test`. Lead only; not measured. | `crates/ecp-analyzer/Cargo.toml:12`. | code read | none |

### DSL providers

| id | tier | problem | evidence | status | goes to |
|---|---|---|---|---|---|
| dsl F1 | T1 | `ALL_PROVIDER_NAMES` has 19 entries against 37 constructors. Overlay, incremental and baseline paths skip bash, hcl, sql, proto, solidity, dockerfile, workflows and compose. Same finding as freshness F3 and index F2. | `crates/ecp-cli/src/reanalyze.rs:34-54,102-104`; `crates/ecp-cli/src/provider_registry.rs:37-83`. `crates/ecp-cli/tests/incremental_full_parity.rs:333` pins the gap. | re-run | 6 |
| dsl F2 | T1 | OpenAPI YAML specs route to the Kubernetes provider and produce an empty graph. `YamlProvider` is unreachable. Tests bypass the dispatch. | `crates/ecp-core/src/analyzer/pipeline.rs:170-186`; `crates/ecp-analyzer/src/kubernetes/parser.rs:284-290`; `crates/ecp-analyzer/tests/openapi_graph_e2e.rs:24`. | code read | 6 |
| dsl F3 | T5, T2 | The markdown and yaml providers are dead, and two tests pin them in place. | `ecp cypher "MATCH (n:File) WHERE n.name ENDS WITH '.md' RETURN count(n)"` returns 0. `crates/ecp-analyzer/src/resolution/builder.rs:1268-1272`. | reviewer repro | 6 |
| dsl F4 | T4 | Five distinct hand-kept path-to-language mappings. `Language::from_normalized_path` returns `Unknown` for `vue`, `astro`, `svelte`, `proto`, `json`. The rename table lacks `luau`, `vue`, `astro`, `svelte`. | `crates/ecp-core/src/analyzer/pipeline.rs:91-194`; `crates/ecp-core/src/file_category.rs:261-321`; `crates/ecp-analyzer/src/identifier_finder/mod.rs:52-88`. | code read | 6 |
| dsl F5 | T1 | Imports of GitHub Actions, Kubernetes, compose, Dockerfile and HCL never become edges. | `MATCH (a)-[r:Imports]->(b) WHERE a.filePath STARTS WITH '.github/' RETURN ...` returns zero rows, although `ci.yml` has `uses: ./.github/actions/diagnose-checkout-failure`. | reviewer repro | none |
| dsl F6 | T1, T3 | 35 GitHub Actions `run: \|` steps share one Decorator node named `run:\|`. | `ecp cypher "MATCH (a)-[r:Decorates]->(b) ..."` returned `Decorates,enable/Enable auto-merge,"run:\|"`. `crates/ecp-analyzer/src/github_actions/parser.rs:157-164,468-470`. | re-run | fix |
| dsl F7 | T1 | A Service and Deployment with the same `metadata.name` in one YAML file share an identity, and the second becomes a tombstone labelled `uid-collision`. Same shape in compose `environment:` and duplicate SFC sections. Not built. | `crates/ecp-analyzer/src/kubernetes/parser.rs:257-274`; `crates/ecp-core/src/uid.rs:33-35`; `crates/ecp-analyzer/src/resolution/builder.rs:1089-1180`. | code read | 2 |
| dsl F8 | T1 | Vue, Svelte and Astro emit no calls. `impact --target fetchUser` omits the `.vue` caller. | `grep -n "extract_calls\|calls" vue/parser.rs svelte/parser.rs astro/parser.rs` finds only `calls: vec![]`. `crates/ecp-analyzer/src/sfc_common.rs:117-258,202-214`. | re-run | 10 |
| dsl F9 | T1 | Compose `build:` mappings are treated as raw text instead of extracting `context`. `depends_on:` mappings and list-form `environment:` are omitted, with no blind spot. | `crates/ecp-analyzer/src/docker_compose/parser.rs:111-117,256-300`. | code read | fix |
| dsl F10 | T4, T5 | Twelve LangSpec providers copy one 170-line loop and never set `owner_class`. | `crates/ecp-analyzer/src/cairo/parser.rs:44-174`; `grep -c 'owner_class: Some\|owner_class,'` gives 0 on all 12. | reviewer repro | 10 |
| dsl F11 | T5 | Three copies of the YAML AST helpers. Two `query` fields are unused behind `#[allow(dead_code)]`. | `crates/ecp-analyzer/src/kubernetes/parser.rs:64-163`; `docker_compose/parser.rs:40-130`. | code read | none |
| dsl F12 | T5 | 37 scratch binaries sit in `ecp-analyzer/src/bin/` (530 lines). | `crates/ecp-analyzer/src/bin/test_*.rs`. | code read | none |
| dsl F13 | T1, T2 | The walker prunes git submodules silently. The cold walk does not skip `.git`. Speculative. | `crates/ecp-cli/src/walker_filter.rs:61-69`; `crates/ecp-cli/src/commands/admin/index.rs:80-90`; `crates/ecp-cli/src/auto_ensure.rs:1357`. | code read | none |

### Secondary commands (group, review, diff, dev, rename, pattern, schema, output)

| id | tier | problem | evidence | status | goes to |
|---|---|---|---|---|---|
| secondary G1 | T1 | Group extractors emit only `ContractRole::Provider`, so `cross_links` is always empty and `group impact` reports zero cross hits. The uid formats also cannot match. | `grep -n "Consumer" cmd/group/extractors/*.rs` returns nothing (11 Provider sites). `crates/ecp-cli/src/commands/group/impact.rs:143-146`; `crates/ecp-cli/tests/group_impact.rs:9-11`. | re-run | 8 |
| secondary RN1 | T1 | `ecp rename parse parse_config` with `a.py:parse` and `b.py:parse` renames both. The documented ambiguity error does not exist. | `grep ambiguous` finds only the header comment at `crates/ecp-cli/src/commands/rename.rs:6`. Targets are collected at `rename.rs:314-345`. | re-run | 5 |
| secondary D1 | T1 | `ecp diff` and `ecp review --since` stash with `-u`, check out a sha and run a bare `stash pop` on the user's tree. A non-zero `pop` is not reported. | `crates/ecp-cli/src/commands/diff/git_guard.rs:33-35,71-81,113-115`; reached from `review/aggregate.rs:424`. | re-run | fix |
| secondary O1 | T3, T1 | `compress_for_llm` strips `uid` on the premise that it is a `kind:path:name` triple. The real uid is a u64 hash. | `crates/ecp-cli/src/output.rs:132-137`; the test at `:525` builds a uid value the code never emits. | re-run | fix |
| secondary DV1 | T5 | `ecp dev` ships 2,100+ lines for maintainers and CI: `pr-analyze` (924), `uid-audit` (526), `verify-resolver` (677). | `crates/ecp-cli/src/commands/dev/mod.rs:1-34`; `crates/ecp-cli/src/commands/impact/payload.rs:5-25` is typed for `pr_analyze`. | code read | filter |
| secondary G2 | T1 | `group find` and `group summary` drop failed members silently. One member without a graph reads as "0 hits". | `crates/ecp-cli/src/commands/group/mod.rs:73-114`; `crates/ecp-cli/src/commands/group/find.rs:164`. | code read | none |
| secondary G3 | T1 | `group impact` hides test and heuristic callers that `ecp impact` shows. `--timeout-ms` is dropped. There is no `--file`. | `crates/ecp-cli/src/commands/impact/symbol.rs:101,106,115`; `crates/ecp-cli/src/commands/group/impact.rs:105`. | code read | none |
| secondary G4 | T1, T3 | `group find --merge rrf` dedups by `"{kind} {name}"`, so distinct symbols merge. Its projection omits `caller_count`. | `crates/ecp-cli/src/commands/group/find.rs:177-205`; `crates/ecp-cli/src/commands/find.rs:1298`. | code read | none |
| secondary R1 | T1 | `ecp review` reports `status: clean` when a constituent failed. Six constituents map `Err` to an empty list. | `crates/ecp-cli/src/commands/review/aggregate.rs:115,169,254,358,426,480`; `review/findings.rs:50-57`. | code read | none |
| secondary R2 | T3, T1 | `ecp review` surfaces maintainer-only blind-spot kinds and reads only the base graph. | `crates/ecp-cli/src/commands/review/aggregate.rs:163-191`. | code read | none |
| secondary R3 | T1 | `review --since` and its impact constituent use different diffs. Confidence 65. | `crates/ecp-cli/src/commands/review/scope.rs:18`; `aggregate.rs:91,275`. | code read | none |
| secondary D2 | T2 | `ecp diff` runs a full resolver-dump index per state for every section. Not timed. | `crates/ecp-cli/src/commands/diff/mod.rs:184-199`. | code read | none |
| secondary D3 | T5 | Diff symbol identity is keyed on `owner_class`, so area 2 must re-choose the key. | `crates/ecp-cli/src/commands/diff/symbols.rs:15-16,45-52`. | code read | none |
| secondary SC1 | T1 | The `ecp schema blindspots` inventory is a hand copy that has drifted. `go-iota-const-block`, `ruby-module-as-enum` and `python-class-as-enum` are missing. | `crates/ecp-cli/src/commands/schema.rs:63-100,507-515`. | code read | none |
| secondary O2 | T3 | Group verbs build text and JSON by hand. `group sync --json` does not escape `contract_id`. | `crates/ecp-cli/src/commands/group/sync.rs:316-327`; `group/impact.rs:202,233`. | code read | none |
| secondary P1 | T5 | `peers_plan` detects a missing symbol by `msg.contains("No symbol named")`. | `crates/ecp-cli/src/commands/peers_plan.rs:68`. | code read | none |
| secondary P2 | T5 | `peer_delta` and `your_overlap_range` are always `None`. | `crates/ecp-cli/src/peer/dispatch.rs:47-48`. | code read | none |
| secondary PT1 | T1 | `ecp pattern` takes candidate files from the base graph, with no caveat. Speculative. | `crates/ecp-cli/src/commands/pattern.rs:207`. | code read | none |

## Small local fixes

Each fix stands alone. The ids come from the area map; the size is the reviewer's size.

| id | change | files | size |
|---|---|---|---|
| frameworks F1 | Take the HTTP verb from the decorator name token, not from a substring of the whole decorator. Skip class-level mapping annotations. | `crates/ecp-analyzer/src/route_detector.rs:63-74` | S |
| flow F1 | Keep the Python relative level when the import is resolved. | `crates/ecp-analyzer/src/flow/module.rs:21-40` | not stated for this part; flow F1 as a whole is M |
| flow F3 | Scope the truncation marker to the slice. Keep the boundaries of cut files in the output. | `crates/ecp-analyzer/src/flow/mod.rs:292-307`; `flow/module.rs:226-235` | M |
| flow F7 | Filter routes first, then count. Say "no shape" when a matching route has no shape. | `crates/ecp-cli/src/commands/shape_check.rs:131-159,286` | S |
| flow F8 | Moved to lane P after C1 and C4: extract the response shape from the handler span, not from the file text. | `crates/ecp-analyzer/src/resolution/builder.rs:1812-1840` | M |
| cli F2 | Change the `inspect` ambiguity hint so it does not point to flags that filter neighbours. | `crates/ecp-cli/src/commands/inspect.rs:740` | S |
| cli F4 | Count the matches left after narrowing, not whether a narrowing flag exists. | `crates/ecp-cli/src/commands/impact/symbol.rs:153` | S |
| cli F7 | Use the `clap` `ValueEnum` for `--format` so an unknown value is rejected. | `crates/ecp-cli/src/output.rs:24-31` | S |
| cli F10 | Group the matches of an ambiguous `inspect`. Move constant keys out of the per-edge payload. | `crates/ecp-cli/src/commands/inspect.rs:669,714-746` | S |
| cli F12 | Return an `Err` instead of `process::exit(1)`. | `crates/ecp-cli/src/commands/find_schema_bindings.rs:229` | S |
| core F2 | Render the uid-collision hint from structured fields. Add the line number. | `crates/ecp-core/src/graph.rs:967-1001` | S |
| core F6 | Name `ecp admin index` in the error text. Remove the `--uid` text. | `crates/ecp-core/src/error.rs:12,17`; `crates/ecp-cli/src/engine.rs:338,345` | S |
| core F9 | Derive the `heuristic` column from `RelType::is_heuristic`. | `crates/ecp-cli/src/commands/schema.rs:284-307` | S |
| secondary D1 | Build the baseline graph from a scratch checkout of the sha. Stop stashing on the user's tree. | `crates/ecp-cli/src/commands/diff/git_guard.rs`; `crates/ecp-cli/src/commands/diff/mod.rs:198` | M |
| secondary O1 | Keep `uid` in the compressed output, or drop it only when `kind`, `filePath` and `name` are present. | `crates/ecp-cli/src/output.rs:129-141` | S |
| dsl F6 | Take the first word of the scalar content for the `run:` decorator. | `crates/ecp-analyzer/src/github_actions/parser.rs:157-164,468-470` | S |
| dsl F9 | Extract `context` from `build:` mappings and handle `depends_on:` mappings. Add a blind spot for a shape the provider does not read. | `crates/ecp-analyzer/src/docker_compose/parser.rs:111-117,256-300` | S to M |
| agent A3 | Make tool exposure opt-in on the command. Remove `ecp_uninstall` and `ecp_usage` from the MCP list. | `crates/ecp-mcp/src/schema.rs:72-95`; `crates/ecp-cli/src/cli.rs:156-166` | S |
| agent A6 | Push a result limit into `compute_hits`. Build only the rows shown. | `crates/ecp-cli/src/commands/find.rs:54,1113-1156`; `crates/ecp-cli/src/commands/hook/pre_tool_use.rs:13-14` | M |
| agent A7 | Remove the git hook calls to `rename-branch` and `prune`. | `crates/ecp-cli/src/commands/hook_watcher.rs:55-60` | S |
| agent A8 | Show the update notice to the human channel, not to `additionalContext`. | `crates/ecp-cli/src/commands/hook/user_prompt_submit.rs` | S |
| agent A10 | Remove the five stub hosts from the menu. | `crates/ecp-cli/src/admin/host_integration/mcp/mod.rs:17-116` | S |
| freshness F6 | Moved to phase 1 (C3's behaviour slice): classify porcelain paths (contract C3) and skip per-path syscalls only for paths that are neither source nor resolution config. A filter on "has a provider" would drop `tsconfig.json`, `go.mod` and `Cargo.toml` changes. | `crates/ecp-cli/src/auto_ensure.rs:443-451,631-665` | S |
| freshness F10 | Write a failed marker for warm-attach and behind-head rebuilds. | `crates/ecp-cli/src/auto_ensure.rs:928-952` | S |
| types-session A5 | Match `Interface` in the Kotlin enclosing-class lookup, after a Kotlin query confirms the defect. | `crates/ecp-analyzer/src/kotlin/receiver_types.rs:477` | S |
| index F11 | Make `ecp admin index` check the builder fingerprint, like `build_l2`. | `crates/ecp-cli/src/commands/admin/index.rs:376-394,481-495` | S |

Not a repo change: the project instruction file `.claude/CLAUDE.md` is gitignored and local; its vector-fallback line has no code behind it.

## Measure before acting

| id | claim | what to measure |
|---|---|---|
| flow F2 | Uncached flow queries took about 3 s on the reviewer's 1077-file repo, because they parse and evaluate the whole corpus; a cache hit took 0.045 s. | The split between `resolve`, the AST scans and evaluation inside the 3 s. The reviewer ran no profile. |
| index F8 | Pass 1 allocates a `String` per node, and pass 2 builds a `Resolver` per file. | An `ECP_PROF=1` run on the dense-TypeScript corpus. The only table is from May (`docs/plans/2026-05-22-cold-ingest-opt.md`). |
| index F9 | Each process pays a full `rkyv::access` walk. | The walk time on the vscode graph. `engine.rs:24-26` says 2-4 ms; `orchestrator.rs:270-273` says 10-50 ms. |
| index F10 | A bm25 hit maps back to the graph by an O(N) node scan. | The scan time per query on a 500k-node graph. The reviewer took no timing. |
| postparse F5 | The language query re-runs for pre-scans, and Python walks the tree eight more times. | A per-pass timer inside `parse_file`. `ECP_PROF=1` gives per-provider totals only. |
| postparse F6 | Thirteen call extractors scan every node per call. | The attach time on a generated file of about 5k functions and 50k calls. |
| freshness F7 | Without a session id, each query is a new session with a full re-parse. | The re-parse cost per query with and without a session id. The shared fallback is a product decision. |

## LLM-utility filter: deletion candidates

| module | lines | what it serves | agent-visible cost |
|---|---|---|---|
| `crates/ecp-cli/src/admin/{mod,menu,indexes,groups,config,diagnostics}.rs`, `commands/admin/config.rs`, `commands/update.rs`, `commands/admin/update_check.rs` | about 835 + 663 + 1964 + 414 | Interactive menus, the config wizard and self-update for humans. | One `update available` line injected by the prompt hook (freshness F11). `admin::gc` and `registry_health` are reused and must stay. |
| `crates/ecp-cli/src/commands/usage.rs`, `usage_render.rs`, `insight.rs`, `telemetry_cli.rs`, `crates/ecp-core/src/telemetry.rs`, `crates/ecp-mcp/src/telemetry.rs` | 341 + 203 + 242 + 382 + 143 + 103 | A human usage dashboard. | An MCP call is recorded twice. `ecp_usage` takes 1,308 B of every tool list (agent A9). |
| `crates/ecp-cli/src/commands/dev/` and `commands/verify_resolver.rs` | 2,127 (924 + 526 + 677) | CI labels (`pr-analyze`), a maintainer audit (`uid-audit`), an oracle benchmark (`verify-resolver`). | `DEV_METRIC_BS_KINDS` plumbing. `ecp review` leaks `uid-collision` (secondary DV1, R2). |
| `crates/ecp-core/src/algorithms/leiden.rs` | 774 | One community tag in `ecp processes`. | About 6% of cold build time in the May table (index F12). |
| `crates/ecp-core/src/graph_query.rs` | 218 | Nothing. Only `build_uid_index` has a production caller. | None; the module doc is false (core F5). |
| `crates/ecp-core/src/algorithms/louvain.rs` | 344 | Nothing. It has no importer. | None (core F11). |
| `crates/ecp-cli/src/incremental.rs` and test-only writer routes | 662 | Nothing in production. | `with_symbol_skip_set` is a skip-set trap for the next reader (freshness F9). |
| `crates/ecp-cli/src/admin/host_integration/native/codex.rs`, `crates/ecp-cli/src/native.rs` | 312 + 208 | A Codex native scaffold that cannot run. | None; five stub hosts show as working in the menu (agent A10). |
| `crates/ecp-analyzer/src/post_process/event_topics.rs`, `ast_test.rs`, `crates/ecp-core/src/analyzer/typescript.rs` | 125 + 34 + 37 | Orphans and a mock provider in non-test code. | None (postparse F10). |
| `crates/ecp-cli/src/build/mode.rs` | 34 | Nothing. Only a test uses it. | None (index T5-2). |

## Healthy modules to keep as they are

| module | why |
|---|---|
| `crates/ecp-cli/src/output.rs` (`emit_with_caveat`, `merge_caveats`) | `emit_with_caveat` and `merge_caveats` provide one `result` channel. A missing caveat does not establish answer correctness. |
| `crates/ecp-cli/src/cli.rs` (`needs_graph`, `repo()`) | Exhaustive matches, so a new command variant is a compile error. Dedupe the lists; keep the exhaustiveness. |
| `crates/ecp-cli/src/main.rs` (`dispatch`) | One recorded exit path; telemetry once. |
| `crates/ecp-cli/src/commands/symbol_id.rs` (`resolve_candidates`) | The right shape with an index-vs-scan contract test. It is under-used, not wrong. |
| `crates/ecp-cli/src/commands/flow.rs`, `flow/sources.rs` | Source-based by design. Skipped files become `unreadable_source` boundaries. Refusal of an unsupported language is explicit. |
| `crates/ecp-cli/src/engine.rs` (`graph()`, `validate_once`) | A 100 MB graph loads in 18 ms. |
| `crates/ecp-core/src/session/merged.rs` (`MergedGraph`) | One overlay-merge seam shared by cypher, impact and find. Do not re-split it. |
| `crates/ecp-cli/src/session/overlay_reader.rs`, `view_cache.rs` | Two functions and a keyed cache. The mtime rule handles edit-then-revert. |
| `crates/ecp-core/src/analyzer/type_ladder.rs` (`TypeFacts`) | One trait shared by index and overlay. |
| `crates/ecp-core/src/analyzer/types.rs` (`CallSite` decode) | One parse site with tests. |
| `crates/ecp-analyzer/src/calls.rs` (shared helpers, sorted sweep) | Extend the helpers; do not add copies. |
| `same_file_pick.rs`, `type_ladder.rs`, `name_ref.rs`, `heritage.rs`, `import_binding.rs` in `crates/ecp-core/src/analyzer/` | One rule behind a facts trait with two adapters is a real seam. Do not merge or inline. |
| `LangSpec` (30 analyzer implementations) and `LanguageProvider` (37 registered analyzer providers; `ecp-core` adds one mock, not counted) | Real seams. |
| `crates/ecp-analyzer/src/function_meta/mod.rs` (`extract_with`) | One walker and one span index; per-language code is only `extract_one`. |
| `crates/ecp-analyzer/src/post_process/{enum_variant_defines,decorates_edges,tx_scope_edges}.rs` | They attach by raw index or by kind and owner. The model for postparse F1. |
| `crates/ecp-analyzer/src/post_process/saga_pairs.rs` | The only pass below 0.7 in its scope; the edge type is heuristic and tagged. |
| `crates/ecp-analyzer/src/path_literal.rs`, `pattern_finder.rs` | Sink confidence is carried in the reason. A language joins only with a proving test. |
| `crates/ecp-core/src/cypher/value.rs`, `error.rs`, `ast.rs` | Collision-free keys; small, no behaviour of their own. |
| `crates/ecp-core/src/cypher/executor.rs` (`AggregateKind`, `eval_scalar_funcall`, `pushdown_where`) | An exhaustive aggregate match, one scalar dispatch point, and a conservative pushdown. All anchored queries ran in 0.02 s. |
| `crates/ecp-core/src/cypher/diagnostics.rs` (`unknown_properties`, `absence_over_calls`) | The right way to tag a heuristic. Extend it (cypher C7). |
| The streaming-vs-materialising oracle tests in `executor.rs` | They pin two evaluators together. Fix the expectations pinned in cypher C1. |
| `crates/ecp-mcp` spawn-only dispatch (`spawn.rs`, `server.rs`, `argv.rs`) | MCP answers are the CLI answers, except the text-mode stale caveat that the spawn path drops (agent A1). Cost is one ~10 ms process per call. Fix agent A1 and A3 at its edges. |
| `crates/ecp-mcp/src/{group,peers,schema_mcp}.rs` schemas | `cli_surface_invariants.rs` checks both directions. |
| `crates/ecp-cli/src/commands/hook/pre_tool_use.rs` (output merge), `common.rs` (`resolve_within_repo`) | Search hits are capped at five hits and 2048 bytes. The combined `additionalContext` object also holds independently bounded flow and peer sections. Repo reads stay inside the repo. |
| `crates/ecp-cli/src/commands/admin/claude_code.rs` | Idempotent hook merge and removal. |
| `crates/ecp-analyzer/src/event_topic/` (`EventTopicConfig`, `extract_event_topics`) | A table-driven seam with 25 adapters. |
| `crates/ecp-analyzer/src/php/laravel_routes.rs`, `ruby/rails_routes.rs` | They model real framework semantics and tag uncertainty with blind spots. |
| `crates/ecp-analyzer/src/fetch_shape/mod.rs` (`normalize_route_path`) | Applied identically to server and client sides. |
| `crates/ecp-cli/src/commands/heuristics.rs`, `find_schema_bindings.rs` | Heuristic rows carry `tier` and `requires_verification`; `checks` is not a shared field across these commands. |
| `crates/ecp-cli/src/git/status/private_index.rs`, `git/status.rs` | Moves `git status` to about 8 ms. Every unproven case declines to the plain command. Do not simplify without its tests. |
| `crates/ecp-cli/src/git_cache.rs`, `parse_cache.rs` | A fixed bug class (linked worktrees) and a documented collision fix. |
| `crates/ecp-cli/src/build/{force,semaphore,dirname_picker}.rs`, `walker_filter.rs` | A documented lock order; constants tied to a measured crash; one `git for-each-ref`; one filter shared by two walks. |
| `crates/ecp-cli/src/build/orchestrator.rs` (`worktree_clean_and_head_matches`) | One definition of "clean" shared with the freshness gate. |
| `crates/ecp-core/src/analyzer/pipeline.rs` (`provider_name_for_path`, give-up path) | The one path-to-provider route. Several parse failures emit `omitted` records. Paths without a registered provider return silently (postparse M-5). |
| `crates/ecp-analyzer/src/resolution/builder.rs` (pass 1 tombstones) | They keep node index equal to prefix-sum position. |
| `crates/ecp-core/src/graph_assembly.rs` | Single owner of the derived CSR, `name_index`, `kind_offsets` and the `call_metas` remap. |
| `NodeKind::ALL` and `RelType::ALL` golden tests | They enforce the append-only rule mechanically. |
| `crates/ecp-core/src/file_category.rs` (`determine_category`, `is_test_path`, `pick_global`) | One classifier and one Tier-3 barrier shared by index and overlay. Only the `Language` table is a problem. |
| `crates/ecp-core/src/registry/lock.rs`, `io.rs`; `uid.rs`; `pool.rs` | Bounded wait with a dead-holder probe; atomic writers; zero-allocation hash. |
| `crates/ecp-core/src/graph_fixture.rs` (`GraphFixture`) | 32 CLI test files use it. The model for tests T-3. |
| `crates/ecp-cli/tests/common/parity.rs` (`CallEdge` identity) | Its identity fields are the right base for the strict harness (tests T-1), but it dedups edges (`:88`, `:178`); the strict harness compares multisets. |
| `KNOWN_DIVERGENCES` pattern in the overlay parity tests | A known gap is asserted to still differ, so closing it turns the test red. |
| `*_support` helper dirs and black-box CLI tests | Shared builders assert on resolved edges. Black-box tests assert the agent-visible output. |
| `crates/ecp-analyzer/src/parse_budget.rs`, `preproc_fallback.rs` | Every provider uses the budget. The fallback documents its precision limits. |
| `crates/ecp-cli/src/provider_registry.rs` | Three guard tests are the right shape. The problem is the second list in `reanalyze.rs`. |
| `crates/ecp-cli/src/commands/format.rs` | One exhaustive `NodeKind` and `RelType` rendering. A new variant breaks the build. |
| `crates/ecp-cli/src/commands/diff/baseline.rs`, `diff/symbols.rs` output tiers | Ref resolution rejects option injection. Tiers follow "never fabricate". |
| `crates/ecp-cli/src/peer/render.rs`, `peer/watcher.rs`, `crates/ecp-core/src/peer/inbox.rs` | A 4 KB cap with a stated loss order. Serde defaults for old entries. |
| `crates/ecp-cli/src/commands/pattern.rs`, `group/storage.rs` | Three distinct outcomes and a bounded worker pool; atomic write with an mmap read path. |

## Blind spots of this review

- The installed binary, not this worktree's build, ran the repros of the freshness and agent-integration reports. Line numbers come from this worktree; behaviour comes from `ecp 0.16.4+a9b0c9c`. The binary may be older than the worktree sources.
- No reviewer ran cargo, a test suite or `ecp admin index`.
- CLI: not read were `update.rs`, `summary.rs`, `routes.rs`, `schema.rs`, `uninstall.rs`, the `peers*.rs` bodies, `admin/*`, `hook/*` and most of `group/*`. The overload behaviour (cli F1), the Rust short-owner divergence and cli F12 were not run. Fifteen early `ecp` calls ran without `ECP_SKIP_BG_REBUILD=1`.
- Types and session: not read were `crates/ecp-core/src/session/view.rs` (6325 lines), `import_scope.rs`, `overlay.rs` and the 14-language tests. `routes`, `processes`, `shape-check`, `tool-map`, `review`, `heuristics`, `diff` and `impact literal` were not run against a dirty repo. The "about 18 of 24" count in types-session B1 is a hand count.
- Flow: about 1000 of 1451 lines of `engine.rs`, `classes.rs`, `php.rs`, `python.rs` and the flow tests were not read. PHP flow was not run. `shape_check` was not run, so flow F7 and F8 are from code. Timings are single runs on one repo. Whether the graph cache is keyed on `ECP_PARSER_FINGERPRINT` (flow F9) was not checked.
- Cypher: the test bodies, `session/view.rs` and `overlay_reader::accepted_fragments` were not read. No overlay query was run, so cypher C11 (overlay part), C14, C16 and C18 are from code. C13 scaling is extrapolated from a code comment. The MCP path was not verified.
- Agent integration: not read were `admin/{config,drop,gc,group,index,sessions}.rs`, `doctor/**`, the `pre_tool_use.rs` shell parser (lines 118-1021, 290+ lines that choose which repo's hits the agent sees) and the MCP test bodies. Agent A1 and A2 were not reproduced end to end. Telemetry cost was not measured.
- Frameworks: no `parser.rs` or `receiver_types.rs` was read line by line. No timing was taken, so every T2 statement is structural. The Ruby and C++ hint drop (frameworks F6) is from code only. The probes left `fw-probe*` index entries under `~/.ecp`.
- Freshness: not read were `git/safe_exec.rs`, most of `private_index.rs` and its tests, and `OverlayView` (`view.rs`). The "second defect" predicted for freshness F4 is a prediction. Windows, macOS and `GIT_*` variables were not checked.
- Index pipeline: no benchmark ran. Timings come from `docs/plans/2026-05-22-cold-ingest-opt.md`, `README.md` and code comments, and the May table is stale. Index F1 was reproduced with git and ripgrep, not with `ecp admin index`. Index F3, F5 and F7 are from code only.
- Analyzer post-parse: no timing of any pass. Postparse F2, F3 and F7 are from code. Postparse F8 has no observed wrong edge. Per-language walk counts other than Python come from a grep inventory.
- parse_file bodies: one labeller, one pass, then a documented rule set. The per-language `receiver_types.rs` and `function_meta` bodies are outside the numbers, so duplication behind those seams may add to them. The report states the head of the worktree as `1c51fdf6`, which is two documentation commits ahead of `8868d659`.
- Core graph: not read were `registry/path.rs`, `dirname.rs`, `repo_meta.rs`, the bodies of `leiden.rs` and `louvain.rs`, and `process_trace.rs` after line 200. No Vue, Svelte or Astro repo was indexed, so core F4 has no end-to-end repro.
- Tests: about 664 test files were sampled by name and grep. The sparse cells in tests T-4 may be covered inside combined files. The fixture binaries were not decoded. No script in `scripts/parity` or `scripts/benchmark` ran.
- DSL providers: this repo has no Kubernetes, compose, Vue or Svelte file, so dsl F7 and F9 are traced from code. Eleven of the twelve LangSpec providers were not read in full. The DSL report counts 38 provider constructors; the main-session re-run and the index report count 37, and this page uses 37.
- Secondary commands: nothing ran. Secondary G1 rests on grep; R3 and D2 are inferences. The overlay awareness of `ecp diff` is not established.
- Findings with no area-map placement carry `none` in "goes to": cli F8, cli Q3, core F3, core F8, agent A5, flow F4-F6 and F9, secondary G2-G4, R1-R3, D2, D3, SC1, O2, PT1, and the T5 rows.
- No T1-T4 finding of any report was dropped. T5 lines without a stable id in a report are not on this page: the dead `let _ = mode;` in `find.rs`, the `mcp.rs` toon stub, duplicate `is_heuristic` and `io.rs` bodies, the write-only `AuditLog`, redundant durability in the graph write, the 650-line `GraphBuilder::build`, and the stale doc pointers in `processes.rs` and `cypher/mod.rs`.
