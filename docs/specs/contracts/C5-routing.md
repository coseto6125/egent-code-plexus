# C5 routing result

Status: phase-1 contract. Types: `crates/ecp-core/src/contracts/routing.rs`. Landed in P1 and P2: the cold index, incremental reanalysis, the session overlay, `impact --baseline` and the C3 freshness gate call `FileRouter` and `SizeCap`. Landed in P3 and P4a: the identifier finder, the pattern grammar, the flow grammar, `file_category::Language` and `FileMeta` read the route. Not yet adopted: the `ParsedLanguage` readers (see the Today section).

Plan row (`docs/specs/2026-10-25-architecture-direction.md`, "Contracts before code"): file format, provider, language after parse, identifier finder, capability set, effective size cap; extension defaults plus ordered path rules. Area 6 owns the fact. Areas 1, 2, 4, 5, 6, 7, 8 and 10 read it.

All `file:line` references are at the head of branch `feat/arch-p1-contracts`.

## Defines

One router turns a repo-relative path into one outcome. Every reader takes its routing facts from that outcome, never from its own extension table.

### `Language`

One variant per parser provider that a path can reach: 36 variants. The 14 mainstream languages are `Language::MAINSTREAM`.

| Invariant | How it holds |
|---|---|
| Every per-language fact is a `match` with no wildcard arm. | `provider`, `extensions`, `display_name`, `capabilities`, `embedded_scripts`, `to_legacy` are exhaustive. A new variant does not compile until each fact is decided. |
| `Language::ALL` lists every variant. | A macro writes the enum and `ALL` from one body. |
| Discriminants are dense and append-only. | `#[repr(u8)]`, explicit values `0..=35`. A test checks density. A slice may store the value per file (see Stored data). |
| One language has one provider. | `provider()`. `from_provider` is its inverse. |
| No extension belongs to two languages. | Test. The first extension of a language is its canonical extension. |
| Extensions are case-sensitive. | `a.RS` is unrouted, as in the cold index today. |
| `yml`, `yaml` and `json` are never extension defaults. | Path rules own them. Test. |
| Every language is reachable by a rule. | Test. |

There is no `Unknown` variant. A path that no rule routes has no language: the outcome is `Unrouted`.

### `PathRule` (ordered)

`FileRouter::route` tries the rules in this order. The first rule that decides wins. `/` and `\` both separate components; empty and `.` components are skipped.

| Order | Rule | Matches | Decides |
|---|---|---|---|
| 0 | `TemplateDir` | a component equal to `__brick__` | `Excluded` |
| 1 | `TemplateInterpolation` | a component that holds `{{` or `}}` | `Excluded` |
| 2 | `DockerfileName` | basename `Dockerfile` or `dockerfile` | `Code`, `Fixed(Dockerfile)` |
| 3 | `ComposeName` | basename `docker-compose.yml`, `docker-compose.yaml`, `compose.yml`, `compose.yaml` | `Yaml`, `Fixed(DockerCompose)` |
| 4 | `GitHubWorkflow` | extension `yml`/`yaml` and adjacent components `.github`, `workflows` at any depth | `Yaml`, `Fixed(GitHubActions)` |
| 5 | `YamlContent` | extension `yml`/`yaml` | `Yaml`, `Sniffed([Kubernetes, OpenApi, Yaml])` |
| 6 | `JsonContent` | extension `json` | `Json`, `Sniffed([OpenApi])` |
| 7 | `Extension` | an extension in `Language::extensions()` | `Code`, `Fixed(language)`; dialect `Tsx` for `tsx` |

The extension follows `std::path::Path::extension`: a dot-file such as `.bashrc` or `.py` has no extension.

### `RouteOutcome`

| Variant | Meaning |
|---|---|
| `Routed(FileRoute)` | The path is source. The parse runs the route's providers. |
| `Excluded(PathRule)` | A rule rejects the path. It is not source. |
| `Unrouted` | No rule routes the path. It is not source. |

### `FileRoute`

Constructed only by `FileRouter::route`. Fields are private.

| Field / method | Type | Invariant |
|---|---|---|
| `rule()` | `PathRule` | The rule that decided. |
| `format()` | `FileFormat` | `Code`, `Yaml` or `Json`: the syntax the path names before the content is read. A sniffed route is never `Code`. |
| `dispatch()` | `Dispatch` | `Fixed(language)`, or `Sniffed(candidates)`: never empty, in content-gate order, with no mainstream language (test). |
| `dialect()` | `Dialect` | `Tsx` only for `.tsx`; else `Standard`. |
| `size_cap()` | `SizeCap` | The router's one cap. Equal for every route of one router. |
| `fixed_language()` | `Option<Language>` | `Some` for `Fixed`, `None` for `Sniffed`. |
| `candidates()`, `providers()` | | The languages and provider names the parse may run, in try order. |
| `capabilities_before_parse()` | `CapabilitySet` | The capabilities of every candidate (intersection). |
| `identifier_finder()` | `IdentifierFinder` | `Supported { language, dialect }` when the fixed language has `Capability::IdentifierFinder`; else `Unsupported`. |
| `after_parse(accepted, script)` | `Result<ParsedLanguage, AfterParseError>` | See below. |
| `key()` | `RouteKey` | Equal for two paths only when the rule, the dialect and the fixed language are equal. See below. |

Because no sniffed candidate is mainstream, a reader that needs only a mainstream language may use `fixed_language()` before the parse.

### `ParsedLanguage` (the language after parse)

| Field | Read by | Invariant |
|---|---|---|
| `file` | tool choice: provider, identifier finder, pattern grammar, capability set | One of the route's candidates. For a sniffed route, the first candidate whose content gate accepted. |
| `resolution` | call normalization, import binding, owner rules, global admission | The embedded script language of a Vue, Svelte or Astro file. Else equal to `file`. |

`after_parse` returns `NotACandidate` when `accepted` is not a candidate. It returns `ScriptNotEmbeddable` when `script` is not in `accepted.embedded_scripts()`. Only Vue, Astro and Svelte embed scripts: TypeScript or JavaScript. A sniffed route whose gates all reject has no `ParsedLanguage`, and the file has no graph.

**Handoff to C2.** C2's `Ecosystem::for_language` (`contracts/workspace.rs:67-106`) takes this `Language` with an exhaustive match. It reads `ParsedLanguage::resolution()`, so a Vue, Svelte or Astro file with a script maps through TypeScript or JavaScript to `Node`. The `Vue`, `Astro` and `Svelte` arms are reached only for a component with no script; the recommendation is `Node` there too, so a caller that passes `file()` by mistake gets the same answer. `Language` has no `Unknown`: an unrouted path has no language and no ecosystem.

**The compatibility rule.** `ParsedLanguage::admits_global(candidate)` is `self.resolution == candidate.resolution`. It is the one language rule of the global (Tier-3) tier. The vendor barrier stays in `file_category::FileMeta`.

### `RouteKey` (fragment identity)

`FileRoute::key()` returns three bytes: the `PathRule` discriminant, the `Dialect` discriminant, and the fixed `Language` discriminant (`0xFF` for a sniffed route). `RouteKey::to_bytes()` gives them for hashing.

| Invariant | How it holds |
|---|---|
| A different route gives a different key. | The key holds every route field that chooses a provider or a grammar. Tests: `deploy.yaml` against `.github/workflows/deploy.yaml`, `a.ts` against `a.tsx`. |
| The same route gives the same key, whatever the directory. | Test: `a.rs` against `b/c.rs`. |
| `0xFF` never names a language. | `Language` has fewer than 255 variants (test). |

An L1 fragment key is the hash of `RouteKey::to_bytes()` and the file content. Not landed: it is the content hash only, so a route collision exists:

- `write_fragment_file` sets `fragment_id = content_hash` (`session/overlay_writer.rs:405-406`), and `write_fragment_from_graph` does the same from the graph's content hash (`:483-484`).
- The bin path is `<fragment_id>.bin` (`:367`), and the publish loop replaces it in order (`:365-369`).
- `deploy.yaml` (kubernetes provider) and `.github/workflows/deploy.yaml` (github-actions provider) with identical bytes produce two different graphs but one bin. The second write wins, and the reader loads it for both manifest entries (`session/overlay_reader.rs:82`).
- An omitted graph carries a zero content hash (`ecp-core/src/analyzer/types.rs:1137`), and the overlay writes a fragment for every graph it gets (`session/overlay_writer.rs:303-314`). So every omitted dirty file shares the fragment id `0000000000000000`. The new key hashes the real content for an omitted file too.

`FragmentFile` holds no path (`session/overlay_writer.rs:120`, built by `fragment_file_from_local_graph` at `:169`), so the route key is enough only if no parser reads more of the path than its route. The adoption slice proves this with a 14-language test: two paths with one route and one content give equal fragment bytes. If the test fails for a language, the key hashes the repo-relative path instead of the route key.

A change to the routing table (a rule, a default extension, a candidate list) also bumps `BUILDER_FINGERPRINT`, so manifests of the old table are re-parsed. The table has no version of its own.

### `Capability` and `CapabilitySet`

`CapabilitySet` is a `u16` bit set: `of`, `contains`, `intersection`, `is_empty`, `iter`, `EMPTY`, `FULL`. `Language::capabilities()` gives the set for every file of the language. A reader that lacks a capability for a file says so in its answer; it never answers empty.

| Capability | Languages in this contract | Today's source |
|---|---|---|
| `IdentifierFinder` | 14 mainstream; Bash, Lua, Crystal, Move, Solidity, Nim, Hcl, Sql, Vyper, Verilog, Cairo, Zig | Landed: the finder dispatches on the route (`identifier_finder/mod.rs:51`) |
| `PatternFinder` | 13 mainstream (not Dart) | Landed: the grammar comes from the route (`pattern_finder.rs:113`) |
| `Flow` | TypeScript, JavaScript, Python, PHP | Landed: the grammar comes from the route (`flow/mod.rs:114`) |
| `FetchShapes` | TypeScript, JavaScript, PHP | `resolution/builder.rs:1302-1309` |
| `FetchConsumer` | TypeScript, JavaScript, Python | `resolution/builder.rs:1314-1323` |
| `FunctionMeta` | 14 mainstream | `crates/ecp-analyzer/src/function_meta/`: one file per mainstream language |
| `IndirectDispatch` | C, C++, JavaScript, Python, Rust, TypeScript | `ecp-cli/src/commands/schema.rs:71` (`indirect_dispatch: Status::Implemented`); `indirect_dispatch` named in those 6 `parser.rs` files only |
| `ToolMapImports` | TypeScript, JavaScript, Python, Go, Rust | `commands/tool_map.rs:295-316` |
| `LexicalIndex` | TypeScript, JavaScript | `ecp-core/src/analyzer/lexical.rs:27-30` |

Capabilities that this slice does not define: call extraction, `Overrides` markers, variant references, local types. See Open points.

### `SizeCap`

| Field | Invariant |
|---|---|
| `bytes` | The per-file byte cap. `admits(len)` is `len <= bytes`. A file over the cap is routed but not parsed, and the reader records it as omitted. |
| `source` | `Env`, `RepoConfig` or `BuiltIn`. |

**Cache validity (target, not landed).** The effective cap is meant to be an input of every cached parse result. A cache compares `bytes()`, never `source()`: one cap from two sources parses the same. Landed in P1: the omission decision reads the route's own cap. The two cache rules below are not implemented; follow-up `FU-2026-10-25-77fc8cac8067` tracks them. Three places read the cap:

| Place | Rule |
|---|---|
| Omission decision (landed in P1) | `admits(len)` on the route's own `size_cap()` (`ecp-core/src/analyzer/pipeline.rs:82`, `:213`). The cold walk calls `size_cap.admits` on the same value (`ecp-cli/src/commands/admin/index.rs:112`). No reader keeps a second cap. |
| Dirty manifest freshness | The manifest records the `bytes()` it was parsed under, beside `builder_fingerprint` (`session/overlay.rs:13-18`). A manifest with another value is stale as a whole, like a fingerprint mismatch: every dirty file is re-parsed. The three manifest readers would check it where they check the fingerprint: `auto_ensure.rs:1316`, `session/overlay_reader.rs:68`, `session/overlay_writer.rs:379`. Not implemented: `DirtyFiles` has no cap field. |
| View cache key | `view_cache::key` (`session/view_cache.rs:35-48`) would hash `bytes()`. Not implemented: the key holds the format, `BUILDER_FINGERPRINT`, the binary, the graph stamp, the node count and the accepted fragments, but no cap. |

The cap is in no cache key. Example: a 2 MiB dirty file is parsed under a 4 MiB cap; the cap then drops to 1 MiB. The manifest entry keeps its mtime, so the freshness gate keeps it (`auto_ensure.rs:1327-1347`), and the view cache key does not change. A one-time `BUILDER_FINGERPRINT` bump cannot cover a later config change. The overlay pipeline cache `pipeline_for_path` (`ecp-cli/src/reanalyze.rs:49`) does key on `bytes()`, but it holds pipelines, not parse results.

`SizeCap::resolve(env, repo_config)` takes a parseable `ECP_MAX_FILE_BYTES` value first, then `[index] max_file_bytes`, then `DEFAULT_MAX_FILE_BYTES` (1 MiB). A malformed env value falls through. This is the precedence of `env_size_cap` and `resolve_size_cap` (`ecp-core/src/config.rs:174-186`). Landed in P1: the cold index (`admin/index.rs:71`), incremental reanalysis (`reanalyze.rs:182`), the overlay writer (`session/overlay_writer.rs:260-262`, `:712`) and `impact --baseline` (`commands/impact/baseline.rs:154`) read one `SizeCap`.

## Landed and remaining

Landed in P1 (`6e888477`) and P2 (`fef7dc24`): the rows marked "landed" below. The other rows describe code that still produces the fact; the adoption slices of areas 1, 2, 4, 7, 8 and 10 move them.

### Where each fact is produced

| Fact | Produced now | Maps to |
|---|---|---|
| Extension to provider | Landed in P1: `Language::extensions()` and `Language::provider()` with `PathRule::Extension` (`routing.rs:139`, `:921`). Before P1, `EXTENSION_TABLE` held 51 rows in `pipeline.rs`. | `json` maps to `JsonContent` |
| Path rules to provider | Landed in P1: `PathRule::ALL` and `PathRule::decide` (`routing.rs:848`, `:921`): Dockerfile, Compose, GitHub Actions, then YAML and JSON content. Before P1, `provider_name_for_path` held them. | `PathRule` 2 to 6 |
| Walk admission | Landed in P1: `should_analyze_path` is `FileRouter::PATH_ONLY.route(..)` returning `Routed` (`admin/index.rs:556-571`). The walk records a non-UTF-8 path that would route as a `path-not-utf8` omission (`:121-125`, `:562`). Before P1, the function kept its own extension list. | `PathRule` 0 and 1; `RouteOutcome::Routed` |
| Provider set, cold index | Landed in P1: `provider_names_for_paths` takes `route.providers()` of each path (`reanalyze.rs:33-43`). Before P1, `detect_needed_providers` derived the set and added `markdown` and `yaml` by hand. | `FileRoute::providers()` |
| Provider set, overlay, reanalysis, baseline | Landed in P2 (overlay) and P1 (reanalysis, baseline): all three call `provider_names_for_paths` or `pipeline_for_path` (`reanalyze.rs:33`, `:49`, `:174`; `commands/impact/baseline.rs:151`). Before, `ALL_PROVIDER_NAMES` held 19 names and dropped the rest. | the same `providers()` |
| Provider constructors | `PROVIDER_CONSTRUCTORS`, `ecp-cli/src/provider_registry.rs:40-86` (37 entries) | 36 map to a `Language` through `from_provider`; `markdown` maps to none |
| Language from path | Landed in P4a: `file_category::Language::from_path` and `from_normalized_path` return `FileRouter::route_ignoring_templates(path)` and `FileRoute::to_legacy()` (`file_category.rs:251-276`, `routing.rs:748`, `:890`). The template rules are skipped: the callers read graph files, and those rules scan every component. An unrouted `md`, `txt` or `rst` path stays `Markdown`. | `ParsedLanguage` |
| Language display | `Language::as_str`, `file_category.rs:327-363` | `Language::display_name()`, equal for every shared variant (test) |
| Global admission | `FileMeta::admits`, `file_category.rs:400-402`: `candidate.language == self.language` | `ParsedLanguage::admits_global` |
| Identifier finder | Landed in P3: `FileRoute::identifier_finder()` on `FileRouter::PATH_ONLY` (`identifier_finder/mod.rs:51`). It returns `None` for a path with no finder; `ecp rename` reports that file in `not_scanned`. Unrouted `nims` and `zon` keep their finder until open point 4 (`:112`). | landed |
| Pattern grammar | Landed in P3: the route's fixed language and `Dialect` (`pattern_finder.rs:113`). | landed |
| Flow grammar | Landed in P3: the route's fixed language and `Dialect` (`flow/mod.rs:114`). Unrouted `mts`, `cts` and `phtml` keep flow until open point 4 (`:140`). The lowering still tests `.py`/`.pyi` and `.php`/`.phtml` suffixes per node (`flow/python.rs:5`, `flow/php.rs:13`); they agree with the grammar choice. | landed |
| Size cap | Landed in P1: `config::resolve_size_cap` reads env, repo config, then the 1 MiB default (`config.rs:174-186`). The walk calls `size_cap.admits` (`admin/index.rs:71`, `:112`); the pipeline calls `route.size_cap().admits` (`pipeline.rs:82`, `:213`). Before P1, the walk cap was 1 MiB and the parse cap was 16 MiB, set by the env var only. | `SizeCap` |

### Other readers that map a path to a language or a provider

| Site | Reads | After adoption |
|---|---|---|
| `resolution/builder.rs:1302` `lang_for_path` | Landed in P1: `FileRouter::PATH_ONLY.route(path)` and `fixed_language()` (`:1304-1305`). Before P1, it called `provider_name_for_path`. | `Capability::FetchShapes` on the route |
| `resolution/builder.rs:1316` `is_consumer_extension` | own list: `ts tsx js jsx mjs cjs py` | `Capability::FetchConsumer` |
| `php/laravel_routes.rs:1655-1659` | Landed in P1: `route.fixed_language() == Some(Language::Php)` on `FileRouter::PATH_ONLY`. Before P1, `provider_name_for_path == "php"`. | `ParsedLanguage::file() == Php` |
| `commands/impact/baseline.rs:151-155` | Landed in P1: `provider_names_for_paths` and `resolve_size_cap`; the full provider set of each route. Before P1, `provider_name_for_path` and the 19-name subset. | landed |
| `reanalyze.rs:174-183` `reanalyze_files` | Landed in P1: the same call and the repo's `SizeCap`. Before P1, it kept a path when `provider_name_for_path` was `Some`. | landed |
| `session/overlay_writer.rs:454-457` `parse_graph` | Landed in P2: `pipeline_for_path(path, size_cap)` with the cap from `session_meta.json`'s worktree (`:260-262`). Before P2, `parse_file_raw` ran with no size cap. | landed |
| `session/overlay_writer.rs:701-716` `extract_symbols` | Landed in P2: it routes by the repo-relative path and calls `pipeline_for_path`. Before P2, it read the basename only, so directory rules failed. Callers: tests only (`ecp impact --target append_dirty`) | landed |
| `ecp-core/src/analyzer/lexical.rs:27-30` | own list, adds `mts`, `cts` | `Capability::LexicalIndex` |
| `commands/tool_map.rs:298-316` | own list | `Capability::ToolMapImports` |
| `commands/group/extractors/mod.rs:117-127` | own map to an ecosystem key | the ecosystem derives from `Language` |
| `commands/rename.rs:96-111` `classify_context` | suffix test: markdown, data, code | `FileFormat`, or `Unrouted` for docs |
| `commands/schema.rs:71` `LANGUAGES` | hand-kept table of blind-spot emitters and `indirect_dispatch`. It lists no rename or pattern support. Its `indirect_dispatch` column agrees with `Capability::IndirectDispatch`. | `Language::capabilities()` |
| `ecp-core/src/analyzer/import_binding.rs:14-21` `fqn_extension` | `Language` to `"java"`, `"kt"`, `"php"` | `extensions()[0]` |
| `commands/impact/ambiguous.rs:130` | `Language::from_path(&format!("x.{ext}"))` | `Language::from_extension` |

`Language::from_path`, `Language::from_normalized_path`, `FileMeta::from_path` and `FileMeta::from_normalized_path` have 69 call lines in production code (test modules excluded) across 13 files. The largest: `resolution/resolver.rs` 22, `session/view.rs` 16, `resolution/builder.rs` 7, `analyzer/type_ladder.rs` 6, `analyzer/reexport.rs` 6. Each one moves to `ParsedLanguage`.

### Value mapping, with each value that has no mapping

| Before P1 | Contract | Difference |
|---|---|---|
| Landed in P1: the former `EXTENSION_TABLE` row, 50 of 51 | `Extension` rule, same provider | none. Test `test_route_every_language_extension_first_provider_is_its_language` (`routing.rs:1452`) walks `Language::ALL` and checks the first provider. |
| `("json", "openapi")` | `JsonContent`, `Sniffed([OpenApi])` | none for the provider |
| other `.yaml` to `kubernetes` | `Sniffed([Kubernetes, OpenApi, Yaml])` (`routing.rs:144`, `:940`) | Landed in P1 for OpenAPI YAML. Generic YAML still gets an empty graph (`kubernetes/parser.rs:284-290`): `FileRoute::providers()` leaves out `Language::Yaml` (`routing.rs:712-717`) until open point 6 lands. Also, `YamlProvider::name()` returns `"YAML"` (`yaml/parser.rs:24-26`) while `Language::Yaml.provider()` returns `"yaml"` (`routing.rs:208`); `FU-2026-10-25-0a44c90c8c30` tracks it. |
| `file_category::Language::Unknown` | no variant: `Unrouted`, or the new variant | `Unknown == Unknown` admits today, so `.vue`, `.svelte`, `.astro`, `.proto` and `.json` candidates admit one another. The contract has no such pairing. |
| `file_category::Language::Markdown` for `md`, `txt`, `rst` | `Unrouted` | No `.md` file is parsed: `Language::from_extension("md")` is `None` (asserted at `routing.rs:1030`), so the router returns `Unrouted` and the pipeline skips the file. Landed in P4a: `file_category` keeps `Markdown` for an unrouted path with these extensions; the value is used only for display (`ecp dev uid-audit`). |
| `file_category::Language::Kubernetes` | `Language::Kubernetes` | Today no path yields it (`from_normalized_path` maps YAML to `Yaml`, `:317`). |
| `x.dockerfile` → `Unknown` | `Dockerfile` | The provider already parses it as Dockerfile. Landed in P4a: `file_category` gives `Dockerfile`. |
| `Vue`, `Astro`, `Svelte`, `Protobuf`, `OpenApi` | new variants; `to_legacy()` gives `Unknown` | |
| `__brick__/…`, `{{…}}/…` | `Excluded` | Landed in P1: the cold walk tests the repo-relative path (`admin/index.rs:101`, `:556`), and every parse path excludes them. Before P1, only the cold walk excluded them, on the absolute path, so a repo under a `__brick__` directory lost every file. Landed in P3: the identifier finder and flow give no answer for an excluded path. `file_category` skips the template rules and keeps the extension language. |
| `.py`, `.bashrc` (dot-files) | `Unrouted` | Landed in P3 and P4a: `file_category` gives `Unknown`, and the finders and flow give no answer. Before, they read `.py` as Python. |
| `.mts`, `.cts` | `Unrouted` | Flow (`flow/mod.rs:140`) and `lexical.rs:29` accept them; the index does not route them. Open point 4. |
| `.phtml`, `.nims`, `.zon` | `Unrouted` | Only flow (`flow/mod.rs:140`) or the identifier finder (`identifier_finder/mod.rs:112`) accepts them, outside the route. Open point 4. |
| `.luau` identifier finder | supported (Lua) | Landed in P3. Before, the finder listed only `lua`, and `ecp rename` dropped an indexed `.luau` file without a notice. |
| `A.RS`, `B.PY` (upper case) | `Unrouted` | Landed in P3 (open point 3): the identifier and pattern finders no longer lowercase, so they route as the index does. |
| 1 MiB walk cap, 16 MiB parse cap | one `SizeCap`, default 1 MiB | Landed in P1. Before P1, a file between 1 and 16 MiB was missing from the index and parsed in the overlay. Open point 1. |
| 19 overlay providers | the full set | Landed in P2: Bash, SQL, HCL, Dockerfile, Solidity, protobuf, GitHub Actions, Compose and 8 more reach the overlay. |

The ledger tests that held this table in `routing.rs` are deleted with the readers they compared. `crates/ecp-core/tests/file_category_routing_agree.rs` checks that `file_category::Language` equals the route's `to_legacy()` for every extension default and path rule. The finder and flow unit tests check that each extension default has the reader exactly when its language has the capability.

## Consumers

| Area | Reads | Stops doing |
|---|---|---|
| 1 (shared ladder) | `ParsedLanguage::resolution()` for call normalization (`resolver.rs:730`, `session/view.rs:2828`); `admits_global` as the one language rule of the global tier (`file_category.rs:400`) | deriving the language from the path on each call; comparing `file_category::Language` values |
| 2 (owner split) | `resolution() == Rust` for Rust owner rules (`resolution/index.rs:635`, `resolution/builder.rs:1264`) | `FileMeta::from_path(...).language` for owner decisions |
| 4 (workspace scope) | `resolution()` in the `Imports` pass (`post_process/imports_edges.rs:152`); C2's `Ecosystem::for_language` (`contracts/workspace.rs:67`) takes `contracts::routing::Language` and is called with `resolution()`. A config file can also be a routed source: `webpack.config.ts` is TypeScript source and a Node config (`workspace.rs:109-110`), so its change triggers a parse and a config invalidation. | its own language lookup; `file_category::Language` in `for_language`. A `.vue` file with a TS script now binds imports by local name (`binds_local`), a behaviour change the area 4 slice approves. |
| 5 (read path) | `RouteOutcome::Routed` marks a dirty path as source for C3; `display_name()` for the `language` field (`commands/find.rs:1335`); `Language::from_extension` for pathspecs (`impact/ambiguous.rs:130`) | parsing an excluded path in the overlay (landed in P2); routing by basename (landed in P2: `overlay_writer.rs:701-716` routes by the repo-relative path) |
| 6 (routing) | owns `routing.rs`. Landed in P1 and P2: the cold walk, the overlay, reanalysis and the baseline call `FileRouter::route` with one `SizeCap`, and `EXTENSION_TABLE`, `provider_name_for_path`, `should_analyze_path`'s list and `ALL_PROVIDER_NAMES` are deleted. Landed in P3 and P4a: `file_category::Language::from_*`, the identifier finder, the pattern grammar and the flow grammar read the route. | Remaining: the open point 4 fallbacks in the finder and flow; Vue, Svelte and Astro in `file_category` (slice P4b) |
| 7 (post-parse edges) | `resolution() == Rust` in scope defines (`post_process/scope_defines.rs:101`) | `Language::from_normalized_path(&file_path_lossy)` there |
| 8 (route record) | `Capability::FetchShapes` and `FetchConsumer` with `file()` (`resolution/builder.rs:1302`, `:1314`); `file() == Php` in `laravel_routes.rs:1654` | `lang_for_path` and `is_consumer_extension` |
| 10 (parse driver) | `file()` picks the per-language spec; `capabilities()` is the spec's declared set; the parser reports the SFC script for `after_parse` | per-parser capability lists |
| 11 (tests) | the test "the routing table of area 6 agrees with every reader" (plan line 446) starts from the ledger tests in `routing.rs` | |
| 3 (snapshot) | none. Ignore files and worktree pruning stay in the source snapshot (`admin/index.rs:81-91`). C5 is a pure function of the path. | |

### Cross-area conflict rows that name C5

| Pair | Requirement | How C5 meets it |
|---|---|---|
| 1-6, 2-6 | one language value and one compatibility rule for call normalization, global admission and Rust owners | `ParsedLanguage::resolution()` and `admits_global`. Open point 7 decides where the value is stored. |
| 4-6, 4-5 | config files have no provider; a filter on "has a provider" hides their changes | C5 never classifies config. `RouteOutcome` says only whether a path is source; C2's `config_ecosystem` (`workspace.rs:583`) says whether it is resolution config. The C3 class is a set of both. `webpack.config.ts`, `jest.config.js`, `tsconfig.json` and `package.json` are both source and config; `Cargo.toml`, `go.mod`, `go.work`, `pyproject.toml` and `*.csproj` are config only (test `test_route_config_file_names_source_or_not`). Open point 12. |
| 3-6 | area 3 defines the snapshot; area 6 reads from it | `FileRouter::route` reads only the path, so it applies to any snapshot. |

## Stored data

| Identifier | Changes when |
|---|---|
| `BUILDER_FINGERPRINT` (`registry/commit_meta.rs:93`) | every slice that changes the routing table or what a route parses to. Landed: schema22 (P1, the cold index on `FileRouter`, sniffed OpenAPI YAML), schema23 (P2, the full provider set in the overlay), schema24 (OpenAPI key lines), schema25 (`path-not-utf8` omissions); now `+schema25`. Still to bump: generic YAML (new nodes). The reason is a fragment that parsed successfully but whose meaning is stale. A `parse_failed` entry needs no bump: the freshness gate already re-parses it (`auto_ensure.rs:1327`). But `config/app.yaml` (no Kubernetes markers) parsed before P1 to an empty kubernetes graph is a successful entry (`parse_failed` is set only when archiving fails, `session/overlay_writer.rs:429`); with an unchanged mtime the gate keeps it (`auto_ensure.rs:1327-1347`), and only the fingerprint mismatch (`auto_ensure.rs:1316`, `session/overlay.rs:101-103`) re-parses it. |
| L1 fragment encoding and dirty manifest entry (`FRAGMENT_FORMAT_V5`, `session/overlay.rs:57`) | the slice that keys fragments by `RouteKey` and content: `FRAGMENT_FORMAT_V6`. The gate and the reader drop a V5 entry (`auto_ensure.rs:1327`, `session/overlay_reader.rs:75`), so every old manifest entry is re-parsed. The bytes also gain the per-file language if open point 7 stores it. |
| Dirty manifest header (`DirtyFiles`, `session/overlay.rs:9-18`) | the slice that makes the cap a cache input: a `size_cap_bytes` field beside `builder_fingerprint`. A manifest without it, or with another value, is stale as a whole. |
| View cache key (`session/view_cache.rs:35-48`) | the same slice: the key hashes `SizeCap::bytes()`. `VIEW_CACHE_FORMAT` (`session/view.rs:1060`) needs no bump, because a changed key only misses. |
| `meta.json` (`CommitBuildMeta`) | the same slice: the L2 build records the cap it ran under. Open point 14. |
| `GRAPH_FORMAT_VERSION` (`graph.rs:36`, now 15) | only if `ParsedLanguage` is stored per file (open point 7): one `file` and one `resolution` byte per file record |

A stored `Language` uses the `#[repr(u8)]` discriminant, which is append-only.

## Open points

| # | Decision | Options | Recommendation |
|---|---|---|---|
| 1 | Default size cap. Decided and landed in P1. | 1 MiB (`config.rs:165`) or 16 MiB (the pre-P1 pipeline default) | 1 MiB. It is the documented default, the omission message names `[index] max_file_bytes` (`admin/index.rs:311`), and the cold index already enforced it, so only the overlay and reanalysis changed. This holds only together with the cache rule of `SizeCap` (Defines): without it a cap change leaves old overlay results in place. |
| 2 | The markdown provider | route `md`/`txt`/`rst`, or delete the provider | Delete it in an area 6 cleanup slice. It is constructed and never routed (`provider_registry.rs:10`), and no LLM query has shown a need. |
| 3 | Extension case. Decided and landed in P3. | case-sensitive, or ASCII case-insensitive | Case-sensitive, as the cold index is. The finders no longer lowercase. |
| 4 | Extensions that one reader accepts: `mts`, `cts`, `phtml`, `nims`, `zon` | add to the defaults, or drop | Add `mts` and `cts` to TypeScript as a capability-addition slice with tests. Drop the others, with this capability loss: `phtml` loses `Flow` (`flow/mod.rs:122`); `nims` loses `IdentifierFinder` (`identifier_finder/mod.rs:79`); `zon` loses `IdentifierFinder` (`:82`). None of the three is indexed today. |
| 5 | TypeScript and JavaScript at the global tier | keep them apart, or one ECMAScript family | Keep apart: today's behaviour. A family change is a separate slice with its own parity run. |
| 6 | Generic YAML gets a graph | route it (plan target), or end the YAML chain at OpenAPI | Route it, after a node-count measurement on this repo and vscode. The measurement has not run, so this recommendation is not yet a pass: the slice merges only with the measured counts. Drop the `Yaml` candidate if the nodes only add noise. |
| 7 | Where `ParsedLanguage` lives | stored per file (format bump), or derived from the path at each read | Store it. The SFC script and the sniffed language are not in the path, and storage removes 69 path re-derivations. |
| 8 | Per-rule size caps | none, or a cap per rule | None until a measurement asks for one. |
| 9 | An SFC with two script blocks of different languages | first block, TypeScript if any, or a `BlindSpot` | The parser reports TypeScript if any block is TypeScript and keeps a `BlindSpot` on every language mismatch, so the answer names the file. The Vue parser already defaults to TypeScript (`vue/parser.rs:174-175`). |
| 10 | The OpenAPI branch inside the YAML provider. Done: commit `419308a3` removed it. | keep, or remove | Remove. `OpenApi` precedes `Yaml` in the chain, and the OpenAPI provider already reads YAML (`openapi/schema_scan.rs:136`). |
| 11 | Template exclusion: a C5 path rule or an area 3 snapshot rule | either | C5, on the repo-relative path. Index and overlay then apply it the same way. Area 3 keeps the ignore files. |
| 12 | A path that is both config (C2) and source (C5), e.g. `tsconfig.json` | one class wins, or the class is a set | A set. C3 widens the invalidation scope and also re-parses the file. |
| 13 | Capabilities not defined here: call extraction (Vue, Svelte, Astro emit none), `Overrides` markers, variant references, local types | add now from the plan's counts, or add with the area 10 inventory | Add with the area 10 inventory, each with a code citation. Until then no language claims these capabilities as supported, and a reader treats their absence as "not supported". |
| 14 | The L2 graph and a cap change | rebuild when the recorded cap differs, or answer with a caveat | Rebuild. A cap change is rare, and the omission `BlindSpot` tells the agent to raise the cap (`admin/index.rs:311-312`), which changes nothing until the next commit. Area 3 owns `meta.json` and decides the field. |
