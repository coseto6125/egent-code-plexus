//! C2 area 4: the scope rows of every ecosystem the index stores
//! (`docs/specs/contracts/C2-workspace.md`, "Area 4"). Each test plants one
//! config layout and asserts the row the import resolver will read through
//! `scope_of`: its root, its ecosystem and its facts (aliases, names,
//! search roots, dependencies, config paths).

use ecp_cli::commands::admin::index::run_analyzer_for_paths;
use ecp_cli::config_parser::{
    find_workspace_configs, is_workspace_config, parse_configs, parse_repo_configs,
};
use ecp_core::contracts::workspace::{
    ConfigPresence, ConfigRead, DependencyTarget, Ecosystem, Membership, NameLookup, Scope,
    ScopeAlias, ScopeId, ScopeMatch, WorkspaceFacts,
};
use ecp_core::graph::ArchivedZeroCopyGraph;
use std::path::Path;

fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("tempdir");
    for (rel, content) in files {
        let path = root.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("a file has a directory")).unwrap();
        std::fs::write(path, content).unwrap();
    }
    root
}

/// The rows `ecp admin index` stores for a tree of `files`.
fn rows_of(root: &Path) -> WorkspaceFacts {
    parse_repo_configs(root, &find_workspace_configs(root)).workspace
}

fn rows(files: &[(&str, &str)]) -> WorkspaceFacts {
    rows_of(repo(files).path())
}

/// The one scope that governs `path`; fails on any other answer.
fn scope_for<'a>(facts: &'a WorkspaceFacts, path: &str, ecosystem: Ecosystem) -> &'a Scope {
    match facts.scope_of(path, ecosystem) {
        ScopeMatch::Unique(id) => &facts.scopes[id.index()],
        other => panic!("{path}: expected one {ecosystem:?} scope, got {other:?}"),
    }
}

fn id_of(facts: &WorkspaceFacts, path: &str, ecosystem: Ecosystem) -> ScopeId {
    match facts.scope_of(path, ecosystem) {
        ScopeMatch::Unique(id) => id,
        other => panic!("{path}: expected one {ecosystem:?} scope, got {other:?}"),
    }
}

fn alias(pattern: &str, replacements: &[&str]) -> ScopeAlias {
    ScopeAlias {
        pattern: pattern.to_string(),
        replacements: replacements.iter().map(|r| r.to_string()).collect(),
    }
}

fn read(path: &str) -> ConfigRead {
    ConfigRead {
        path: path.to_string(),
        presence: ConfigPresence::Read,
    }
}

fn missing(path: &str) -> ConfigRead {
    ConfigRead {
        path: path.to_string(),
        presence: ConfigPresence::Missing,
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The names of `id`, as `(name, rank)` debug pairs, sorted.
fn names_of(facts: &WorkspaceFacts, id: ScopeId) -> Vec<String> {
    let mut names: Vec<String> = facts
        .names
        .iter()
        .filter(|entry| entry.scope == id)
        .map(|entry| format!("{}:{:?}", entry.name, entry.rank))
        .collect();
    names.sort();
    names
}

/// Today's repo-wide alias rows, as the scope rows spell them.
fn alias_table(root: &Path) -> Vec<ScopeAlias> {
    parse_configs(root)
        .entries()
        .map(|(pattern, replacements)| ScopeAlias {
            pattern: pattern.to_string(),
            replacements: replacements.to_vec(),
        })
        .collect()
}

// ─── Node ──────────────────────────────────────────────────────────────────

/// The FU-2026-10-25-47047c61252e repro: two packages each map `@/*` to
/// their own `src/*`, beside a root `tsconfig.json` with the same pattern.
/// Each package's row carries its own alias; the root row keeps the root's.
#[test]
fn test_parse_configs_two_packages_each_tsconfig_two_scopes() {
    let tsconfig = r#"{ "compilerOptions": { "paths": { "@/*": ["src/*"] } } }"#;
    let facts = rows(&[
        ("tsconfig.json", tsconfig),
        ("packages/a/package.json", r#"{ "name": "@repo/a" }"#),
        ("packages/a/tsconfig.json", tsconfig),
        ("packages/b/package.json", r#"{ "name": "@repo/b" }"#),
        ("packages/b/tsconfig.json", tsconfig),
    ]);
    let a = scope_for(&facts, "packages/a/src/main.ts", Ecosystem::Node);
    assert_eq!(a.root, "packages/a");
    assert_eq!(a.aliases, [alias("@/*", &["packages/a/src/*"])]);
    assert_eq!(
        a.config_files,
        [
            read("packages/a/package.json"),
            read("packages/a/tsconfig.json")
        ],
        "one row per directory: package.json and tsconfig.json are one scope"
    );
    let b = scope_for(&facts, "packages/b/src/main.ts", Ecosystem::Node);
    assert_eq!(b.root, "packages/b");
    assert_eq!(b.aliases, [alias("@/*", &["packages/b/src/*"])]);
    let root = scope_for(&facts, "src/util.ts", Ecosystem::Node);
    assert_eq!(root.root, "");
    assert_eq!(root.aliases, [alias("@/*", &["src/*"])]);
    assert_eq!(
        facts.lookup_name(Ecosystem::Node, "@repo/a"),
        NameLookup::Unique(id_of(&facts, "packages/a/x.ts", Ecosystem::Node))
    );
    assert_eq!(
        facts
            .scopes
            .iter()
            .filter(|s| s.ecosystem == Ecosystem::Node)
            .count(),
        3
    );
}

/// The root scope's aliases are today's `alias_table` rows, byte for byte
/// and in order (C2 `Scope.aliases`): R-A4b can move the reader to the
/// rows without changing a root edge.
#[test]
fn test_parse_configs_node_root_scope_aliases_equal_alias_table() {
    let dir = repo(&[
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@/*": ["./src/*", "lib/*"] } } }"#,
        ),
        (
            "package.json",
            r##"{ "name": "app", "imports": { "#utils/*": "./src/utils/*" } }"##,
        ),
        (
            "jsconfig.json",
            r#"{ "compilerOptions": { "paths": { "~/*": ["./web/*"] } } }"#,
        ),
        (
            "jest.config.json",
            r#"{ "moduleNameMapper": { "^@test/(.*)$": "<rootDir>/test/$1" } }"#,
        ),
        (
            "webpack.config.js",
            "module.exports = { resolve: { alias: { '@w': path.resolve(__dirname, 'src/w') } } };",
        ),
    ]);
    let facts = rows_of(dir.path());
    let root = scope_for(&facts, "src/index.ts", Ecosystem::Node);
    let expected = alias_table(dir.path());
    assert_eq!(expected.len(), 5, "the fixture plants one alias per config");
    assert_eq!(root.aliases, expected);
}

/// `extends` folds the chain into the row: the inherited `paths` apply,
/// relative to the config that declared them, and every file of the chain
/// is a config path, the absent one as `Missing`.
#[test]
fn test_parse_configs_node_tsconfig_extends_inherits_paths_and_lists_missing() {
    let facts = rows(&[
        (
            "tsconfig.base.json",
            r#"{ "compilerOptions": { "paths": { "@shared/*": ["shared/*"] } } }"#,
        ),
        (
            "packages/a/tsconfig.json",
            r#"{ "extends": ["../../tsconfig.base.json", "./local"] }"#,
        ),
    ]);
    let a = scope_for(&facts, "packages/a/index.ts", Ecosystem::Node);
    assert_eq!(a.aliases, [alias("@shared/*", &["shared/*"])]);
    assert_eq!(
        a.config_files,
        [
            read("packages/a/tsconfig.json"),
            read("tsconfig.base.json"),
            missing("packages/a/local.json"),
        ]
    );
}

/// An `extends` cycle ends at the first repeat: each file is read once and
/// the paths the cycle declares still apply.
#[test]
fn test_parse_configs_node_tsconfig_extends_cycle_reads_each_file_once() {
    let facts = rows(&[
        ("a/tsconfig.json", r#"{ "extends": "./b.json" }"#),
        (
            "a/b.json",
            r#"{ "extends": "./tsconfig.json", "compilerOptions": { "paths": { "~/*": ["lib/*"] } } }"#,
        ),
    ]);
    let a = scope_for(&facts, "a/x.ts", Ecosystem::Node);
    assert_eq!(a.config_files, [read("a/tsconfig.json"), read("a/b.json")]);
    assert_eq!(a.aliases, [alias("~/*", &["a/lib/*"])]);
}

/// A string where `paths` wants a list is skipped, as today's root parse
/// skips it; a non-string item of a list is skipped, its siblings kept.
#[test]
fn test_parse_configs_node_paths_scalar_entry_skipped_list_items_filtered() {
    let facts = rows(&[(
        "pkg/tsconfig.json",
        r#"{ "compilerOptions": { "paths": { "@/*": "src/*" } } }"#,
    )]);
    assert!(scope_for(&facts, "pkg/x.ts", Ecosystem::Node)
        .aliases
        .is_empty());
    let facts = rows(&[(
        "pkg/tsconfig.json",
        r#"{ "compilerOptions": { "paths": { "~/*": [["x/*"], "lib/*", 3] } } }"#,
    )]);
    assert_eq!(
        scope_for(&facts, "pkg/x.ts", Ecosystem::Node).aliases,
        [alias("~/*", &["pkg/lib/*"])]
    );
}

/// `paths` are relative to `baseUrl` when one is set, and `baseUrl` is the
/// scope's search root.
#[test]
fn test_parse_configs_node_base_url_paths_join_base_url() {
    let facts = rows(&[(
        "packages/a/tsconfig.json",
        r#"{ "compilerOptions": { "baseUrl": "./src", "paths": { "@/*": ["*"] } } }"#,
    )]);
    let a = scope_for(&facts, "packages/a/src/x.ts", Ecosystem::Node);
    assert_eq!(a.aliases, [alias("@/*", &["packages/a/src/*"])]);
    assert_eq!(a.search_roots, strings(&["packages/a/src"]));
}

/// A package with no `tsconfig.json` of its own reads the nearest
/// ancestor's, as `tsc` does, and lists it as a config path.
#[test]
fn test_parse_configs_node_package_without_tsconfig_reads_ancestor() {
    let facts = rows(&[
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@/*": ["src/*"] } } }"#,
        ),
        ("packages/x/package.json", r#"{ "name": "x" }"#),
    ]);
    let x = scope_for(&facts, "packages/x/index.ts", Ecosystem::Node);
    assert_eq!(x.root, "packages/x");
    assert_eq!(x.aliases, [alias("@/*", &["src/*"])]);
    assert_eq!(
        x.config_files,
        [read("packages/x/package.json"), read("tsconfig.json")]
    );
}

/// A `package.json` with no `name`, or a blank one, still makes a scope;
/// it carries no name.
#[test]
fn test_parse_configs_node_package_without_name_scope_has_no_name() {
    let facts = rows(&[
        ("a/package.json", r#"{ "version": "1.0.0" }"#),
        ("b/package.json", r#"{ "name": "   " }"#),
    ]);
    let a = id_of(&facts, "a/index.js", Ecosystem::Node);
    let b = id_of(&facts, "b/index.js", Ecosystem::Node);
    assert_eq!(facts.scopes[a.index()].root, "a");
    assert_eq!(facts.scopes[b.index()].root, "b");
    assert!(facts.names.is_empty(), "{:?}", facts.names);
}

/// An empty file and invalid JSON are read configs with no facts: the row
/// exists, so the file's later fix makes it stale.
#[test]
fn test_parse_configs_node_empty_and_invalid_json_scope_without_facts() {
    let facts = rows(&[("a/package.json", ""), ("b/tsconfig.json", "{ not json")]);
    let a = scope_for(&facts, "a/x.js", Ecosystem::Node);
    assert_eq!(a.config_files, [read("a/package.json")]);
    assert!(a.aliases.is_empty());
    let b = scope_for(&facts, "b/x.ts", Ecosystem::Node);
    assert_eq!(b.config_files, [read("b/tsconfig.json")]);
    assert!(b.aliases.is_empty());
    assert!(facts.names.is_empty());
}

/// `tsconfig.json` allows comments and trailing commas.
#[test]
fn test_parse_configs_node_tsconfig_with_comments_reads_paths() {
    let facts = rows(&[(
        "web/tsconfig.json",
        "{\n  // the app alias\n  \"compilerOptions\": { /* map */ \"paths\": { \"@/*\": [\"src/*\",], }, },\n}\n",
    )]);
    assert_eq!(
        scope_for(&facts, "web/x.ts", Ecosystem::Node).aliases,
        [alias("@/*", &["web/src/*"])]
    );
}

/// In a `workspaces` layout a bare version of a member's name is that
/// member; a registry package is `External`; a member lists the root
/// `package.json` that names it. A package outside the workspace keeps the
/// registry copy.
#[test]
fn test_parse_configs_node_workspace_dependency_binds_member() {
    let facts = rows(&[
        ("package.json", r#"{ "workspaces": ["packages/*"] }"#),
        (
            "packages/a/package.json",
            r#"{ "name": "@repo/a", "dependencies": { "@repo/b": "^1.0.0", "react": "^18" } }"#,
        ),
        ("packages/b/package.json", r#"{ "name": "@repo/b" }"#),
        (
            "tools/solo/package.json",
            r#"{ "name": "solo", "devDependencies": { "@repo/b": "^1.0.0" } }"#,
        ),
    ]);
    let b = id_of(&facts, "packages/b/index.ts", Ecosystem::Node);
    let a = scope_for(&facts, "packages/a/index.ts", Ecosystem::Node);
    assert_eq!(
        a.dependency("@repo/b"),
        Some(DependencyTarget::Workspace(b))
    );
    assert_eq!(a.dependency("react"), Some(DependencyTarget::External));
    assert_eq!(
        a.config_files,
        [read("packages/a/package.json"), read("package.json")]
    );
    let solo = scope_for(&facts, "tools/solo/index.ts", Ecosystem::Node);
    assert_eq!(solo.dependency("@repo/b"), Some(DependencyTarget::External));
}

/// A `file:` dependency names a directory of the repo, by its manifest.
#[test]
fn test_parse_configs_node_file_dependency_binds_by_manifest() {
    let facts = rows(&[
        (
            "app/package.json",
            r#"{ "name": "app", "dependencies": { "shared": "file:../shared" } }"#,
        ),
        ("shared/package.json", r#"{ "name": "shared-lib" }"#),
    ]);
    let shared = id_of(&facts, "shared/index.ts", Ecosystem::Node);
    assert_eq!(
        scope_for(&facts, "app/index.ts", Ecosystem::Node).dependency("shared"),
        Some(DependencyTarget::Workspace(shared))
    );
}

/// The entry fields of a `package.json`, source first, resolved in the
/// package.
#[test]
fn test_parse_configs_node_entry_fields_become_entry_files() {
    let facts = rows(&[(
        "lib/package.json",
        r#"{ "name": "lib", "main": "./dist/index.js", "source": "src/index.ts" }"#,
    )]);
    assert_eq!(
        scope_for(&facts, "lib/src/index.ts", Ecosystem::Node).entry_files,
        strings(&["lib/src/index.ts", "lib/dist/index.js"])
    );
}

/// A config under `node_modules/` or `vendor/` belongs to an installed
/// dependency and makes no scope.
#[test]
fn test_parse_configs_vendored_config_makes_no_scope() {
    assert!(!is_workspace_config("node_modules/lib/package.json"));
    assert!(!is_workspace_config("web/node_modules/lib/tsconfig.json"));
    assert!(!is_workspace_config("vendor/acme/lib/composer.json"));
    assert!(is_workspace_config("packages/a/package.json"));
    assert!(
        !is_workspace_config("Cargo.toml"),
        "the module tree owns Cargo"
    );
    let facts = rows(&[
        ("node_modules/lib/package.json", r#"{ "name": "lib" }"#),
        ("vendor/acme/lib/composer.json", r#"{ "name": "acme/lib" }"#),
    ]);
    assert_eq!(facts, WorkspaceFacts::default());
}

/// Paths the walk did not hand over in normal form, or that vanished
/// before the read, make no scope and no error.
#[test]
fn test_parse_repo_configs_unreadable_or_unnormal_paths_skipped() {
    let dir = repo(&[("a/go.mod", "module example.com/a\n")]);
    let paths = strings(&["gone/go.mod", "./a/go.mod", "a\\go.mod", ""]);
    assert_eq!(
        parse_repo_configs(dir.path(), &paths).workspace,
        WorkspaceFacts::default()
    );
    assert_eq!(
        parse_repo_configs(dir.path(), &[]).workspace,
        WorkspaceFacts::default()
    );
}

/// Two callers that read one tree at once get the same rows.
#[test]
fn test_parse_repo_configs_concurrent_calls_return_same_rows() {
    let dir = repo(&[
        ("go.mod", "module example.com/m\n"),
        ("web/package.json", r#"{ "name": "web" }"#),
    ]);
    let root = dir.path();
    let (one, two) = std::thread::scope(|s| {
        let one = s.spawn(|| rows_of(root));
        let two = s.spawn(|| rows_of(root));
        (one.join().unwrap(), two.join().unwrap())
    });
    assert_eq!(one, two);
    assert_eq!(one.scopes.len(), 2);
}

// ─── Go ────────────────────────────────────────────────────────────────────

/// The innermost `go.mod` governs a file. Each module path is a name and
/// two aliases joined to its root; the root module's stay verbatim, as
/// today's `alias_table` writes them.
#[test]
fn test_scope_of_nested_go_mod_returns_inner() {
    let dir = repo(&[
        ("go.mod", "module example.com/root\n\ngo 1.22\n"),
        (
            "svc/go.mod",
            "module \"example.com/root/svc\" // the service\n",
        ),
    ]);
    let facts = rows_of(dir.path());
    let svc = scope_for(&facts, "svc/handler/h.go", Ecosystem::Go);
    assert_eq!(svc.root, "svc");
    assert_eq!(
        svc.aliases,
        [
            alias("example.com/root/svc", &["svc"]),
            alias("example.com/root/svc/*", &["svc/*"]),
        ]
    );
    let root = scope_for(&facts, "cmd/main.go", Ecosystem::Go);
    assert_eq!(root.root, "");
    assert_eq!(root.aliases, alias_table(dir.path()));
    assert_eq!(
        facts.lookup_name(Ecosystem::Go, "example.com/root/svc"),
        NameLookup::Unique(id_of(&facts, "svc/x.go", Ecosystem::Go))
    );
}

/// A `go.work` lists itself in each module it uses; a used directory with
/// no `go.mod` makes no scope and is a `Missing` config of the members.
#[test]
fn test_parse_configs_go_work_missing_module_listed_missing() {
    let facts = rows(&[
        ("go.work", "go 1.22\n\nuse (\n\t./a\n\t./gone\n)\n"),
        ("a/go.mod", "module example.com/a\n"),
    ]);
    let a = scope_for(&facts, "a/x.go", Ecosystem::Go);
    assert_eq!(
        a.config_files,
        [read("a/go.mod"), read("go.work"), missing("gone/go.mod")]
    );
    assert_eq!(
        facts.scope_of("gone/x.go", Ecosystem::Go),
        ScopeMatch::Unscoped
    );
}

/// A `replace` to a local directory binds that module; any other
/// `require` binds an in-repo module by path, else is `External`.
#[test]
fn test_parse_configs_go_replace_local_binds_workspace_dependency() {
    let facts = rows(&[
        (
            "a/go.mod",
            "module example.com/a\n\nrequire (\n\texample.com/b v0.0.0\n\tgithub.com/x/y v1.2.3 // indirect\n)\n\nreplace example.com/b => ../b\n",
        ),
        ("b/go.mod", "module example.com/b\n"),
    ]);
    let b = id_of(&facts, "b/x.go", Ecosystem::Go);
    let a = scope_for(&facts, "a/x.go", Ecosystem::Go);
    assert_eq!(
        a.dependency("example.com/b"),
        Some(DependencyTarget::Workspace(b))
    );
    assert_eq!(
        a.dependency("github.com/x/y"),
        Some(DependencyTarget::External)
    );
}

/// An empty `go.mod` is a module root with no module path.
#[test]
fn test_parse_configs_go_empty_go_mod_scope_without_name() {
    let facts = rows(&[("go.mod", "")]);
    let root = scope_for(&facts, "main.go", Ecosystem::Go);
    assert_eq!(root.config_files, [read("go.mod")]);
    assert!(root.aliases.is_empty());
    assert!(facts.names.is_empty());
}

// ─── Python ────────────────────────────────────────────────────────────────

/// A project root: today's alias joined to the root, the import spelling of
/// the name as `Primary`, and the declared package root as a search root
/// (a string where setuptools also takes a list).
#[test]
fn test_parse_configs_python_project_root_scope_names_and_search_roots() {
    let facts = rows(&[(
        "svc/pyproject.toml",
        "[project]\nname = \"my-pkg\"\n\n[tool.setuptools.packages.find]\nwhere = \"src\"\n",
    )]);
    let id = id_of(&facts, "svc/src/my_pkg/core.py", Ecosystem::Python);
    let svc = &facts.scopes[id.index()];
    assert_eq!(svc.root, "svc");
    assert_eq!(
        svc.aliases,
        [alias("my-pkg/*", &["svc/src/my-pkg/*", "svc/my-pkg/*"])]
    );
    assert_eq!(svc.search_roots, strings(&["svc/src"]));
    assert_eq!(names_of(&facts, id), ["my-pkg:Alternate", "my_pkg:Primary"]);
}

/// The root project's aliases are today's `alias_table` rows; with no
/// declared package root the project root is the search root.
#[test]
fn test_parse_configs_python_root_scope_aliases_equal_alias_table() {
    let dir = repo(&[("pyproject.toml", "[tool.poetry]\nname = \"tool\"\n")]);
    let facts = rows_of(dir.path());
    let root = scope_for(&facts, "tool/cli.py", Ecosystem::Python);
    assert_eq!(root.aliases, alias_table(dir.path()));
    assert_eq!(root.search_roots, strings(&[""]));
}

/// Invalid TOML is a read config with no facts.
#[test]
fn test_parse_configs_python_invalid_toml_scope_without_facts() {
    let facts = rows(&[("lib/pyproject.toml", "[project\nname = ")]);
    let lib = scope_for(&facts, "lib/x.py", Ecosystem::Python);
    assert_eq!(lib.config_files, [read("lib/pyproject.toml")]);
    assert!(lib.aliases.is_empty());
    assert_eq!(lib.search_roots, strings(&["lib"]));
    assert!(facts.names.is_empty());
}

// ─── C# ────────────────────────────────────────────────────────────────────

/// A project directory: `RootNamespace` is the name code writes, a
/// `ProjectReference` binds the referenced project, and a reference to an
/// absent project is `External` and a `Missing` config.
#[test]
fn test_parse_configs_dotnet_project_dir_scope_and_project_reference() {
    let facts = rows(&[
        (
            "src/App/App.csproj",
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><RootNamespace>Company.App</RootNamespace></PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="..\Lib\Lib.csproj" />
    <ProjectReference Include="..\Gone\Gone.csproj" />
  </ItemGroup>
</Project>"#,
        ),
        (
            "src/Lib/Lib.csproj",
            r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><AssemblyName>Company.Lib</AssemblyName></PropertyGroup></Project>"#,
        ),
    ]);
    let lib = id_of(&facts, "src/Lib/Util.cs", Ecosystem::DotNet);
    let app_id = id_of(&facts, "src/App/Program.cs", Ecosystem::DotNet);
    let app = &facts.scopes[app_id.index()];
    assert_eq!(app.root, "src/App");
    assert_eq!(app.membership, Membership::Subtree);
    assert_eq!(
        app.dependency("Lib"),
        Some(DependencyTarget::Workspace(lib))
    );
    assert_eq!(app.dependency("Gone"), Some(DependencyTarget::External));
    assert_eq!(
        app.config_files,
        [read("src/App/App.csproj"), missing("src/Gone/Gone.csproj")]
    );
    assert_eq!(
        names_of(&facts, app_id),
        ["App:Alternate", "Company.App:Primary"]
    );
    assert_eq!(
        facts.lookup_name(Ecosystem::DotNet, "Company.Lib"),
        NameLookup::Unique(lib)
    );
}

/// Two projects in one directory are two scopes, and a file both govern
/// is ambiguous (C2: never resolve it with one picked silently).
#[test]
fn test_parse_configs_dotnet_two_projects_one_dir_scope_of_ambiguous() {
    let sdk = r#"<Project Sdk="Microsoft.NET.Sdk"></Project>"#;
    let facts = rows(&[("svc/App.csproj", sdk), ("svc/Tests.csproj", sdk)]);
    let ScopeMatch::Ambiguous(ids) = facts.scope_of("svc/X.cs", Ecosystem::DotNet) else {
        panic!("two projects in one directory must be ambiguous");
    };
    let manifests: Vec<&str> = ids
        .iter()
        .map(|id| facts.scopes[id.index()].manifest())
        .collect();
    assert_eq!(manifests, ["svc/App.csproj", "svc/Tests.csproj"]);
}

/// A project without an SDK compiles the files it lists, no others.
#[test]
fn test_parse_configs_dotnet_legacy_project_listed_membership() {
    let facts = rows(&[(
        "legacy/Legacy.csproj",
        r#"<Project ToolsVersion="15.0">
  <ItemGroup>
    <Compile Include="Sub\Util.cs" />
    <Compile Include="Program.cs" />
    <Compile Include="..\Outside.cs" />
  </ItemGroup>
</Project>"#,
    )]);
    let legacy = scope_for(&facts, "legacy/Program.cs", Ecosystem::DotNet);
    assert_eq!(
        legacy.membership,
        Membership::Listed(strings(&["legacy/Program.cs", "legacy/Sub/Util.cs"]))
    );
    assert_eq!(
        facts.scope_of("legacy/Other.cs", Ecosystem::DotNet),
        ScopeMatch::Unscoped
    );
}

/// The `.csproj` scan matches the extension in any case, but `App.CSPROJ`
/// is no C2 manifest name: it makes no row, and every other row stays.
#[test]
fn test_parse_configs_dotnet_uppercase_extension_skipped_other_rows_kept() {
    let facts = rows(&[
        (
            "svc/App.CSPROJ",
            r#"<Project Sdk="Microsoft.NET.Sdk"></Project>"#,
        ),
        ("web/package.json", r#"{ "name": "web" }"#),
    ]);
    assert_eq!(
        facts.scope_of("svc/X.cs", Ecosystem::DotNet),
        ScopeMatch::Unscoped
    );
    assert_eq!(scope_for(&facts, "web/x.ts", Ecosystem::Node).root, "web");
}

// ─── PHP ───────────────────────────────────────────────────────────────────

/// A package root: each PSR-4 prefix is an alias from the namespace (with
/// `/` for `\`) to its directories, a list or one string; `require` keys
/// bind in-repo packages and skip platform packages.
#[test]
fn test_parse_configs_composer_package_root_psr4_aliases() {
    let facts = rows(&[
        (
            "lib/composer.json",
            r#"{
  "name": "acme/lib",
  "autoload": { "psr-4": { "Acme\\Lib\\": "src/" } },
  "autoload-dev": { "psr-4": { "Acme\\Lib\\Tests\\": ["tests/", "more-tests"] } },
  "require": { "php": ">=8.1", "ext-json": "*", "acme/core": "^1.0", "monolog/monolog": "^3" }
}"#,
        ),
        (
            "core/composer.json",
            r#"{ "name": "acme/core", "autoload": { "psr-4": { "": "src/" } } }"#,
        ),
    ]);
    let core = id_of(&facts, "core/src/Core.php", Ecosystem::Composer);
    let lib_id = id_of(&facts, "lib/src/Thing.php", Ecosystem::Composer);
    let lib = &facts.scopes[lib_id.index()];
    assert_eq!(lib.root, "lib");
    assert_eq!(
        lib.aliases,
        [
            alias("Acme/Lib/*", &["lib/src/*"]),
            alias("Acme/Lib/Tests/*", &["lib/tests/*", "lib/more-tests/*"]),
        ]
    );
    assert_eq!(
        lib.dependency("acme/core"),
        Some(DependencyTarget::Workspace(core))
    );
    assert_eq!(
        lib.dependency("monolog/monolog"),
        Some(DependencyTarget::External)
    );
    assert_eq!(lib.dependency("php"), None);
    assert_eq!(lib.dependency("ext-json"), None);
    assert_eq!(
        facts.scopes[core.index()].aliases,
        [alias("*", &["core/src/*"])],
        "an empty prefix is the fallback directory"
    );
    assert_eq!(
        facts.lookup_name(Ecosystem::Composer, "acme/lib"),
        NameLookup::Unique(lib_id)
    );
}

// ─── Swift ─────────────────────────────────────────────────────────────────

/// A package root: each target is a name and an alias to its directory
/// (`path:`, else `Sources/` or `Tests/`); a commented-out target is not;
/// a `.package(path:)` dependency binds the local package.
#[test]
fn test_parse_configs_swiftpm_package_root_target_aliases() {
    let facts = rows(&[
        (
            "Pkg/Package.swift",
            r#"// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Pkg",
    dependencies: [
        .package(path: "../Other"),
        .package(url: "https://github.com/apple/swift-log.git", from: "1.0.0"),
    ],
    targets: [
        .target(name: "Core"),
        // .target(name: "Old"),
        .target(name: "Net", dependencies: ["Core"], path: "Src/Net"),
        .testTarget(name: "CoreTests", dependencies: ["Core"]),
    ]
)
"#,
        ),
        (
            "Other/Package.swift",
            r#"let package = Package(name: "Other", targets: [.target(name: "OtherKit")])"#,
        ),
    ]);
    let other = id_of(&facts, "Other/Sources/OtherKit/a.swift", Ecosystem::SwiftPm);
    let pkg_id = id_of(&facts, "Pkg/Sources/Core/a.swift", Ecosystem::SwiftPm);
    let pkg = &facts.scopes[pkg_id.index()];
    assert_eq!(pkg.root, "Pkg");
    assert_eq!(
        pkg.aliases,
        [
            alias("Core", &["Pkg/Sources/Core"]),
            alias("Net", &["Pkg/Src/Net"]),
            alias("CoreTests", &["Pkg/Tests/CoreTests"]),
        ]
    );
    assert_eq!(
        names_of(&facts, pkg_id),
        [
            "Core:Primary",
            "CoreTests:Primary",
            "Net:Primary",
            "Pkg:Alternate"
        ]
    );
    assert_eq!(
        pkg.dependency("Other"),
        Some(DependencyTarget::Workspace(other))
    );
    assert_eq!(
        facts.lookup_name(Ecosystem::SwiftPm, "OtherKit"),
        NameLookup::Unique(other)
    );
}

// ─── Index build ───────────────────────────────────────────────────────────

/// `ecp admin index` stores the rows of every ecosystem beside the Cargo
/// rows, and the Cargo ids do not move: the overlay's crate heads read
/// them by id. The archived table reads back whole.
#[test]
fn test_run_analyzer_for_paths_stores_cargo_and_config_rows() {
    let dir = repo(&[
        (
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        ),
        ("src/lib.rs", "pub fn run() {}\n"),
        ("go.mod", "module example.com/app\n"),
        ("main.go", "package main\n\nfunc main() {}\n"),
        ("packages/a/package.json", r#"{ "name": "@repo/a" }"#),
        ("packages/a/index.ts", "export function a() {}\n"),
    ]);
    let out = tempfile::tempdir().unwrap();
    let graph = run_analyzer_for_paths(dir.path(), out.path(), None, None)
        .expect("cold index")
        .1;
    let facts = &graph.workspace;
    assert_eq!(facts.scopes[0].ecosystem, Ecosystem::Cargo);
    assert_eq!(
        facts.lookup_name(Ecosystem::Cargo, "app"),
        NameLookup::Unique(ScopeId(0))
    );
    assert_eq!(
        scope_for(facts, "packages/a/index.ts", Ecosystem::Node).root,
        "packages/a"
    );
    assert_eq!(
        facts.lookup_name(Ecosystem::Go, "example.com/app"),
        NameLookup::Unique(id_of(facts, "main.go", Ecosystem::Go))
    );
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(&archived.workspace_facts(), facts);
}
