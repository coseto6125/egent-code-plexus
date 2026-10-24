# C2: workspace facts

Status: contract, phase 1. Types: `crates/ecp-core/src/contracts/workspace.rs`. No code calls them yet.

Plan row (`docs/specs/2026-10-25-architecture-direction.md`, "Contracts before code"): resolution scope per file, scope root, effective config, dependency map; stored once, read by index, overlay, flow and the `Imports` pass. Consumers: area 1 (the Rust subset in B's first round), area 4, area 5 (the invalidation closure).

Every `file:line` below was read on branch `feat/arch-p1-contracts` at `1d267693`.

## Defines

### Path rule

Every path in the table is repo-relative and `/`-separated. It has no leading `./`, no trailing `/`, no `.` or `..` segment and no `\`. The repo root directory is the empty string. `WorkspaceFacts::validate` rejects any other path in a root, a config file, an entry file or a search root. Alias replacements are exempt; see `Scope.aliases`.

### Types

| Type | Kind | Purpose |
|---|---|---|
| `Ecosystem` | stored enum, `u8` | The package ecosystem whose config files make a scope: `Cargo`, `Node`, `Go`, `Python`, `DotNet`. |
| `ConfigRole` | enum | What a config file name does: `Root` can make its directory a scope root; `Contributing` only adds to the configs that read it. |
| `ScopeId` | stored `u32` | Index of a row in `WorkspaceFacts.scopes`. |
| `Scope` | stored row | One resolution scope: root, config files, effective config, dependency map. |
| `ScopeAlias` | stored row | One `(pattern, replacements)` entry of a scope's alias table. |
| `Dependency`, `DependencyTarget` | stored row, enum | One declared dependency: `Workspace(ScopeId)` or `External`. |
| `ScopeName`, `NameRank` | stored row, enum | One name by which code reaches a scope; `Primary` or `Alternate`. |
| `WorkspaceFacts` | stored table | The whole fact set of one commit. |
| `NameLookup` | result | `Unique(ScopeId)`, `Ambiguous { count }` or `Missing`. |
| `Invalidation`, `RescopedSubtree` | result | The scopes that one changed path makes stale. |
| `WorkspaceFactsError` | result | The invariant that a table breaks. |

### Fields and invariants

| Field | Invariant |
|---|---|
| `Ecosystem` variants | Append-only. The discriminant is stored. A new variant changes no layout. An older binary rejects it, so it ships with a `BUILDER_FINGERPRINT` bump. |
| `Scope.ecosystem`, `Scope.root` | `(ecosystem, root)` is unique. `scopes` is strictly sorted by that pair. |
| `Scope.config_files` | Never empty. The first file is the manifest that made `root` a scope, and it is directly in `root`. The other files are every file whose bytes the effective config read: a workspace `Cargo.toml`, a `go.work`, an `extends` target, an ancestor `tsconfig.json`. A change to any of them makes the scope stale. |
| `Scope.entry_files` | The files that a path through the scope's name starts from, best first. Cargo: the lib entry (`[lib] path`, else `src/lib.rs`, else `src/main.rs`), then the bin roots. May be empty. |
| `Scope.aliases` | The effective path aliases, in declaration order. That is the order the resolver tries them. Each entry has at least one replacement. Replacements are repo-relative, like today's `PathAliases` rows. The producer joins the scope root in front of a scope-relative replacement. It keeps a root-scope replacement verbatim, so today's rows stay byte-identical. |
| `Scope.search_roots` | Directories that a specifier which is neither relative nor aliased is searched in, in order: a Python source root, a TS `baseUrl`, an include directory. Empty when the ecosystem searches none. B's first round writes none. |
| `Scope.dependencies` | The dependency map. Strictly sorted by `name`. A `Workspace` target is in range and has the same ecosystem. `External` means: a path under this name must not bind to a same-name project file. |
| `ScopeName` | `names` is strictly sorted by `(ecosystem, name, rank, scope)`. A name is not empty. `ecosystem` equals the ecosystem of `scope`. Two entries can share a name. |
| `NameRank` | `Primary` is the name code writes: a Cargo `[lib] name` (else the package name with `-` replaced by `_`), an npm package name, a Go module path. `Alternate` is another accepted spelling. It answers only when no `Primary` entry has the name. |

### Functions

All functions are pure. None reads the file system.

| Function | Rule |
|---|---|
| `Ecosystem::for_language(Language)` | The ecosystem whose scopes resolve a file of that language. The `match` has no wildcard arm, so a new `Language` variant does not compile until someone maps it. |
| `Ecosystem::classify_config(file_name)` | The ecosystem and role of a config file by its name alone (table below). |
| `WorkspaceFacts::new`, `validate` | `new` returns the table only when every invariant above holds. A reader runs `validate` once on a table it did not build. |
| `scope_at(ecosystem, root)` | The scope whose root is exactly `root`. |
| `scope_of(path, ecosystem)` | **The resolution scope of a file.** It is the scope of that ecosystem whose root is the deepest directory that contains the file. `None` when no root contains it. The rule reads only the path, so an indexed file and a new dirty file get the same answer. |
| `lookup_name(ecosystem, name)` | The scope that a name reaches, across the whole table. A `Primary` entry beats an `Alternate` one. Two entries of the best rank give `Ambiguous` with the true count. |
| `Scope::dependency(name)` | The declared dependency of that name. |
| `Scope::path_aliases()` | The scope's aliases as today's `PathAliases` expander, in declaration order. |
| `config_ecosystem(path)` | The ecosystem of a path when it is a resolution config: its file name is a config name, or a scope lists it in `config_files`. |
| `invalidated_by(path)` | The scopes that a change to the path makes stale (rules below). |
| `dependents_of_name(ecosystem, name)` | The scopes that declare a dependency of that name, whatever it resolves to today. |

Config file names:

| File name | Ecosystem | Role |
|---|---|---|
| `Cargo.toml` | Cargo | Root |
| `package.json`, `tsconfig.json`, `jsconfig.json` | Node | Root |
| `jest.config.json`, `jest.config.js`, `jest.config.ts`, `webpack.config.js`, `webpack.config.ts` | Node | Contributing |
| `go.mod` | Go | Root |
| `go.work` | Go | Contributing |
| `pyproject.toml` | Python | Root |
| `*.csproj` | DotNet | Root |

These are the files that `parse_configs` reads today. A file such as `tsconfig.base.json` is a config only because a scope lists it.

`invalidated_by(path)` collects, in this order:

1. every scope that lists `path` in `config_files`;
2. when the file name is a config name of ecosystem E in directory D: the scope of E that contains D, and every scope of E whose root is D or below D;
3. one hop of dependents: every scope with a `Workspace` dependency on a scope from steps 1 and 2.

When the name has the `Root` role, the result also carries `RescopedSubtree { E, D }`: every file of E under D can change scope, and that includes a file that has no scope today. A changed scope can also gain a name that another scope declares as `External`. The consumer reads the new config, then calls `dependents_of_name` for each new name.

## Today

### Where each fact is produced and read

| Fact | Produced today | Read today |
|---|---|---|
| Effective config (aliases) | `parse_configs` (`ecp-cli/src/config_parser.rs:9`), called once per index (`commands/admin/index.rs:297`) and given to the builder (`:301`). It reads the repo root only: `tsconfig.json` `:13`, `package.json` `:37`, `go.mod` `:50`, `Cargo.toml` `:67`, `pyproject.toml` `:91`, `jsconfig.json` `:110`, jest `:134`, `:144`, webpack `:155`. It walks the tree for `*.csproj` only (`:163`, `parse_csproj_files` `:452`). | Builder field `path_aliases` (`resolution/builder.rs:160`), stored by `record_path_aliases` (`:386`) into `alias_table` (`ecp-core/src/graph.rs:1269`, row type `PathAliasFact` `:845`). Index resolver field (`resolution/resolver.rs:365`), used through `for_each_specifier_candidate` (`:464`; `ecp-core/src/analyzer/reexport.rs:1358`). Overlay: `ArchivedZeroCopyGraph::path_aliases` (`graph.rs:1427`) into `OverlayFacts.aliases` (`session/view.rs:1663`, `:1819`), expanded in `for_each_candidate` (`:2596-2615`) and `relative_module_rank` (`:5159`). |
| Alias expander | `PathAliases` (`ecp-core/src/analyzer/path_aliases.rs:31`), one list for the repo. | `expand(specifier, visit)` (`path_aliases.rs:59`) takes the specifier alone, with no caller scope. |
| Scope root | Rust only: `RustWorkspaceModTree.crates`, `crate_name → (crate_dir, …)` (`ecp-analyzer/src/rust/module_tree.rs:158`), built by `build` (`:183`) from the root `Cargo.toml` only (`:194`) and its `[workspace] members` (`parse_workspace_members` `:930`). Every other ecosystem has one implicit root: the repo root. | Index Tier 3.5: `try_module_tree_resolve` (`resolver.rs:3037`) through `mod_tree_resolve` (`:3054`) and `anchored_rust_module` (`:674`). The tree is built in `GraphBuilder::build` (`builder.rs:360`). |
| Scope per file | Rust only: `file_to_crate` (`module_tree.rs:161`), keyed by canonical absolute path. | `crate_for_file` (`module_tree.rs:449`) for `crate::`, `self::`, `super::`. |
| Crate name | `build` (`module_tree.rs:229-252`): the `[lib] name`, else the package name with `-` → `_`, wins. The package spellings fill only keys that no other crate claims. | `resolve_fqn` (`module_tree.rs:268`). A `<crate_name>::` head reaches any workspace member (`:292`), with no dependency check. |
| Entry files | `find_crate_entry` (`module_tree.rs:1026`), `bin_roots` (`:711`). | The module tree walk. |
| Overlay Rust facts | `RustModules::new` (`session/view.rs:4241-4277`) keeps as roots only the target roots in directories that contain a dirty caller. It holds no crate table (`:4234-4238`). It is built only when a dirty Rust file has a `::` call (`:506-517`). | `resolve_rust_module_callee` (`view.rs:4406`, called at `:684` and `:4836`). |
| Dependency map | None. `read_crate_manifest` reads `[package] name`, `[lib] name` and `[lib] path` only (`module_tree.rs:1015-1024`). `go.mod` `require` and `replace` are not read. | None. |
| `Imports` pass facts | Its own: a basename index and a directory-component index over all files (`post_process/imports_edges.rs:92-112`). | Steps 3a-3f (`:238-397`). Step 3d matches a Go import by last segment and caller extension (`:301-309`). Step 3e is a third Rust module-path rule (`:324-356`). Step 3f matches a C# or Swift namespace by directory name (`:363-383`). Every module-step hit is emitted at 0.9 (`:391`; the barrel edge also at `:215`). The language comes from the path (`:152`). |
| Flow facts | None. `ecp flow` loads no graph (`commands/flow.rs:1`). | `module::resolve` (`flow/module.rs:21-43`) joins a specifier to the importer's directory only. It has no alias, no scope and no absolute-import search. Called from `flow/mod.rs:332` and `flow/module.rs:73`. |
| Config change in a dirty view | None. Config files have no provider: `.toml` and `go.mod` have no row, and `.json` goes to the OpenAPI provider (`ecp-core/src/analyzer/pipeline.rs:144-146`, `provider_name_for_path` `:159-194`). | `reanalyze_files` returns no graph when no dirty path has a provider (`ecp-cli/src/reanalyze.rs:186-192`). L1 fragments carry symbols, imports and export aliases only (`session/overlay_writer.rs:119-123`). |

### Mapping to the new types

| Today's value | New value |
|---|---|
| `alias_table` rows from root configs | `Scope.aliases` of the root scope of each ecosystem, verbatim. Area 4 moves them. |
| `RustWorkspaceModTree.crates` key (code name) | `ScopeName { Cargo, name, Primary, scope }` |
| `crates` key (package spelling, `fallbacks`) | `ScopeName { Cargo, name, Alternate, scope }`, written only for the first member that claims it, as `module_tree.rs:244-252` does. |
| `crates` value `.0` (`crate_dir`, absolute) | `Scope.root`, made repo-relative. |
| `find_crate_entry` result, then `bin_roots` | `Scope.entry_files` |
| Root `Cargo.toml` and member `Cargo.toml` | `Scope.config_files` = member manifest first, then the root manifest. |
| `file_to_crate` (crate of a file) | `scope_of(path, Cargo)` |
| csproj `ProjectReference` alias (`config_parser.rs:166-189`) | `Dependency { stem, Workspace(project scope) }` in area 4. |
| Go `module` alias (`config_parser.rs:57-59`) | `ScopeName { Go, module path, Primary }`, plus the alias row with the scope root joined. |

Values with no mapping:

| Today's value | Why it has no row |
|---|---|
| `workspace_canon` and the canonical path strings (`module_tree.rs:150`, `:158`) | The table holds repo-relative paths only. Canonical paths depend on the machine. |
| `ModTree`, `ReExportMap`, `targets`, `file_targets` (`module_tree.rs:121`, `:144`, `:164-167`) | They come from source files, not from config. They stay with area 1's module-tree facts. |
| Cargo alias rows `name/*` → `src/*` and `name` → `src/lib.rs`, `src/main.rs` (`config_parser.rs:79-84`) | `names` plus `entry_files` replace them. They stay in `alias_table` until area 4, so index edges do not change in B's first round. |
| pyproject `name/*` → `src/name/*`, `name/*` (`config_parser.rs:98-102`) | Area 4 decides between this alias and `search_roots`. Note: the reader takes any line that starts with `name` in any TOML section (`:92-94`), not only `[project]`. |
| `CsprojMeta`, `ComposerJsonMeta`, `SwiftPackageMeta`, `GlobalJsonMeta`, `NugetConfigMeta` (`config_parser.rs:360-423`) | `parse_global_json` (`:521`), `parse_nuget_config` (`:535`), `parse_single_composer_json` (`:565`) and `parse_single_swift_package` (`:606`) have no production caller. The file allows dead code (`:1-4`). Area 4 adds `Ecosystem` variants for them. |
| `[dependencies]` of a crate | Not read today. B's first round can fill `Scope.dependencies` (recommended); the lookup does not use them in that round (Open point 3). |

## Consumers

### Area 1: B's first round, the Rust subset (B7b's persistence half)

Reads:

- `lookup_name(Cargo, head)` for the head of an `other_crate::path::f()` call, then `entry_files[0]` of that scope as the crate head.
- `scope_of(caller, Cargo)` to find the caller's crate.

Writes: one `Scope` row per workspace member, from the same manifest parse that `RustWorkspaceModTree::build` does (`module_tree.rs:183-256`). The rows use `Ecosystem::Cargo` only. `aliases` and `search_roots` stay empty.

Must stop:

- `RustModules` must stop treating a crate outside the dirty callers' directories as unknown (`view.rs:4246-4270`).
- No second reader of `Cargo.toml` may appear. The index and the stored rows come from one parse.

### Area 4: resolve an import in the caller's workspace scope

Reads:

- `scope_of(caller, Ecosystem::for_language(language))`, then `Scope::path_aliases()`, `search_roots` and `dependency(name)`.
- The `Imports` pass and `ecp flow` read the same rows.

Writes: rows of every other ecosystem, found by walking the tree for the config names above. A row change needs no layout change.

Must stop:

- `parse_configs` reading the repo root only (`config_parser.rs:13-160`).
- `PathAliases::expand` without the caller's scope (`path_aliases.rs:59`, callers `resolver.rs:464`, `view.rs:2611`, `:5172`).
- The `Imports` pass guessing by unique basename or directory and emitting 0.9 (`imports_edges.rs:296-309`, `:363-383`, `:391`). A guess takes C4's heuristic evidence class, below 0.7.
- The third Rust module-path rule in the `Imports` pass (`imports_edges.rs:324-356`).
- `ecp flow` resolving a module relative to the importer only (`flow/module.rs:21-43`).

### Area 5: one read path, and the invalidation closure

Reads:

- `config_ecosystem(path)` for C3's "resolution config" class of each porcelain path.
- `invalidated_by(path)` for each dirty config path, then `dependents_of_name` for each new name in the reread config. The clean files to resolve again are the files whose `scope_of` is in the result, and the files of `RescopedSubtree`.

Must stop:

- `reanalyze_files` dropping a dirty set that has no provider (`reanalyze.rs:186-192`). A config path reaches the overlay as a config, not as a parse input.
- Any filter on "has a provider" that also decides "is a change". Config files have no provider (`pipeline.rs:144-146`).

### Cross-area conflict rows that name C2

| Pair | Requirement | Status |
|---|---|---|
| 1, 4 | B's first round stores the Rust subset with one `graph.bin` change. Area 4 adds rows, not layout. | Satisfied. `WorkspaceFacts` is one new field. Cargo rows use the generic columns. Another ecosystem is a new `Ecosystem` variant and new rows. |
| 4, 5 | A config change widens the invalidation scope; C3's "resolution config" class. | Satisfied by `config_ecosystem`, `invalidated_by`, `RescopedSubtree` and `dependents_of_name`. |
| 4, 6 | Config files have no provider (`pipeline.rs:146`). | Satisfied: the config class is by file name and by table rows, never by provider. `package.json` and `tsconfig.json` also route to the OpenAPI provider, so one path can be a parse input and a config. Open point 7 names the owner. |
| 3, 4 | Area 3 defines the source snapshot; areas 4 and 6 read from it. | Requirement on the producer, open until area 3 lands (FU-2026-10-25-8aae9b180d81). The contract types read no file. Today the readers read the disk under the index root: `config_parser.rs:13`, `module_tree.rs:194`, `:210`, `:980`. The L2 build is keyed by SHA (`build/orchestrator.rs:134-140`). A read from the disk can therefore put an uncommitted config into a SHA build; this is inferred from the code, not reproduced. |

## Stored data

| Identifier | B's first round (Rust rows) | Area 4 (other ecosystems) | Area 5 (dirty config) |
|---|---|---|---|
| `GRAPH_FORMAT_VERSION` (`graph.rs:36`, now 15) | +1: one field `workspace: WorkspaceFacts`, appended after `route_middleware` (`graph.rs:1277`). Take the next free number at merge time; A-B also bumps. | none: rows only. A new `Ecosystem` variant is no layout change. | none |
| `BUILDER_FINGERPRINT` (`registry/commit_meta.rs:79`, now `schema21`) | bump: new stored data | bump: rows change edges | none |
| L1 fragment encoding (`overlay_writer.rs:119-123`) | none | none | none with Open point 6, option (a) |
| Dirty manifest | none | none | Config paths must enter the dirty set. C3 owns that record. |
| View cache key (`session/view_cache.rs:35-48`) | none: the overlay reads the base rows only | none | `inputs_hash` must include the content hash of each dirty config file. |

Old graphs: a graph without the field fails the version check and rebuilds. Proof per the repo `CLAUDE.md`: read a graph written by the previous release and assert that the rebuild leaves no old graph behind.

## Open points

1. **Per-file scope: a rule or a stored column.** The plan says "resolution scope per file". (a) The rule `scope_of(path, ecosystem)`, as typed: no column, and a new dirty file gets the same answer as an indexed one. (b) A dense `file_scope: Vec<u32>` keyed by file index: O(1), but a second source of truth, and no value for a file the index did not see. Recommendation: (a). Add a per-view memo if a profile shows the cost.
2. **Duplicate primary names.** Today the last member with a code name wins, with no marker (`module_tree.rs:249`). `lookup_name` returns `Ambiguous`. (a) B's first round writes only the last claimant, so the overlay matches the index. (b) Both sides adopt `Ambiguous` and a baseline file lists the changed edges. Recommendation: (a) in the first round, (b) in area 4.
3. **Dependency-restricted lookup.** Today any member name resolves (`module_tree.rs:292`). The contract carries the dependency map. Recommendation: the first round keeps the workspace-wide lookup on both sides. Area 4 switches both sides to "caller's dependencies first" in one slice.
4. **Fallback to an ancestor alias table.** codegraph tries the nearest config, then the root map. The contract folds every inherited file into the scope at build time, and it has no query-time chain. Recommendation: no fallback for a scope with its own config that does not extend the root. If a later slice adds one, its hit carries its own reason and a confidence below 0.7.
5. **`ecp flow` loads no graph** (`commands/flow.rs:1`). (a) Flow maps `graph.bin` and reads the table. (b) Flow runs the same producer over the config files at its ref. Recommendation: (b). The rule stays "one producer", and flow keeps its no-index promise.
6. **Dirty config in the overlay.** (a) Build the dirty rows per view from the dirty config bytes. (b) Store them in a new L1 fragment. Recommendation: (a). The rows are few, and (a) changes no stored format.
7. **Who classifies a config path.** C3 needs the class, and C5 owns routing. (a) C3 calls `config_ecosystem`. (b) C5's routing result gets a config row. Recommendation: (a). C5 stays the provider table, and `Ecosystem::for_language` moves to C5's language-after-parse type when C5 lands.
8. **Include and exclude rules.** `tsconfig.json` `include`/`exclude`, Cargo `exclude` and non-member crates are not modelled; the deepest root wins. A rule for them would need a new column, which is a layout change. Recommendation: decide before B's first round freezes the layout. The default is to accept the deepest-root rule: a non-member crate with its own `Cargo.toml` becomes its own scope in area 4.
9. **`alias_table` after area 4.** Area 4 moves every alias into scope rows. Deleting `alias_table` is a layout change. Recommendation: area 4 stops reading it and writes it empty. Delete the field at the next format bump that another slice makes.
