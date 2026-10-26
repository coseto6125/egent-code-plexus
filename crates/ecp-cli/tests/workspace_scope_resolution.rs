//! R-A4b (C2 area 4): an import resolves in the caller's workspace scope.
//!
//! The caller's scope is the `Unique` C2 row of its language's ecosystem
//! that admits it (`WorkspaceFacts::unique_scope_of`). The scope's own
//! aliases replace the repo-wide alias table, then its workspace
//! dependencies map `name` and `name/*` to the target scope
//! (`PathAliases::expand_in`). A scope with no alias of its own, an
//! ambiguous scope and a file in no scope keep the repo-wide reading. Where
//! several source roots or namespace files hold an absolute import's module,
//! the one under the caller's scope root is the module
//! (`module_discovery::discover_module`).
//!
//! Each case plants one tree and runs it through the production index and
//! the overlay of a dirty copy of the caller (`common/strict_parity.rs`). It
//! asserts what a reader of the full index gets: the caller's `Calls`
//! target file, its confidence and reason, and, for TypeScript, the
//! caller's `Imports` edge. Then the overlay's `Calls` and `Imports` edges
//! must equal the index's, by endpoints and exact confidence (the overlay
//! stamps its own reason, so the parity leaves reasons out).
//!
//! | ecosystem | scoped case | no-scope case |
//! |---|---|---|
//! | Node (TypeScript, JavaScript) | `@/*` per package, root `tsconfig.json` beside it, `package.json` name, alias outside the scope, invalid config, one name on two scopes | relative import |
//! | Go | nested `go.mod` | same-package call |
//! | Python | two `pyproject.toml` projects, one package `app` | one package |
//! | PHP | two Composer packages, one namespace | no `composer.json` |
//! | C# | the csproj `ProjectReference` still binds; project namesake ignored, see below | no `.csproj` |
//!
//! Languages of the 14 this file leaves out, and why no scope row changes
//! their binding (`PathAliases::scope_of` maps their language to no
//! ecosystem):
//!
//! ```text
//! workspace_scope_resolution java no C2 row today (R-A4a writes none for Maven or Gradle)
//! workspace_scope_resolution kotlin no C2 row today (R-A4a writes none for Gradle)
//! workspace_scope_resolution rust Cargo rows are read by the crate table (R1c, R11b), not by the alias reading
//! workspace_scope_resolution ruby no C2 row today (R-A4a writes none for Bundler)
//! workspace_scope_resolution swift R-A4a writes SwiftPm rows, but this slice reads none of them: Swift keeps the repo-wide reading
//! workspace_scope_resolution c no C2 row today
//! workspace_scope_resolution cpp no C2 row today
//! workspace_scope_resolution dart no C2 row today (R-A4a writes none for pubspec.yaml)
//! ```

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_core::analyzer::types::NEAREST_COPY_REASON;
use ecp_core::graph::RelType;
use ecp_core::session::merged::MergedGraph;
use strict_parity::{run, Edit, Observed, Scenario};

/// The lowest confidence of a tier that binds through an import or its
/// module: `QualifierScoped` and `TypeOwned` (0.85); `ImportScoped` is 0.95.
/// The path-proximity and nearest-copy fallbacks bind at 0.6.
const SCOPED: f32 = 0.85;
/// `Tier::ImportScoped`: the TS / JS named-import tier.
const IMPORT_SCOPED: f32 = 0.95;

/// One edge of a graph, by its endpoints' files and names.
#[derive(Debug, Clone)]
struct Bound {
    source_file: String,
    source: String,
    target_file: String,
    target: String,
    confidence: f32,
    reason: String,
}

/// Every `rel` edge of the full index of `observed`.
fn index_edges(observed: &Observed, rel: RelType) -> Vec<Bound> {
    let graph = observed.full();
    let merged = MergedGraph::new(graph, None);
    merged
        .all_edges()
        .filter(|edge| edge.rel_type() == rel)
        .filter_map(|edge| {
            let source = merged.node(edge.source)?;
            let target = merged.node(edge.target)?;
            Some(Bound {
                source_file: source.file_path(&merged)?.to_string(),
                source: source.name(&merged).to_string(),
                target_file: target.file_path(&merged)?.to_string(),
                target: target.name(&merged).to_string(),
                confidence: edge.confidence(),
                reason: edge.reason(graph).to_string(),
            })
        })
        .collect()
}

/// The production index of `files` and the overlay of `dirty`, unchanged
/// but for a trailing newline.
fn observe(name: &str, files: &[(&str, &str)], dirty: &str) -> Observed {
    let source = files
        .iter()
        .find(|(path, _)| *path == dirty)
        .map(|(_, source)| *source)
        .expect("the dirty file is one of the files");
    let edited = format!("{source}\n");
    run(&Scenario {
        name,
        owner: "R-A4b (workspace scope resolution)",
        files,
        edits: &[Edit::Write(dirty, &edited)],
    })
}

/// The `Calls` edges from `caller` in `file` to a node named `callee`.
fn calls(observed: &Observed, file: &str, caller: &str, callee: &str) -> Vec<Bound> {
    index_edges(observed, RelType::Calls)
        .into_iter()
        .filter(|e| e.source_file == file && e.source == caller && e.target == callee)
        .collect()
}

/// The index binds `caller`'s call of `callee` to `want` alone, at a tier
/// that reads the import (`>= min`), and not by the nearest-copy guess.
fn assert_scoped_call(
    observed: &Observed,
    file: &str,
    caller: &str,
    callee: &str,
    want: &str,
    min: f32,
) {
    let found = calls(observed, file, caller, callee);
    let targets: Vec<&str> = found.iter().map(|e| e.target_file.as_str()).collect();
    assert_eq!(targets, [want], "{file} {caller} -> {callee}: {found:?}");
    let edge = &found[0];
    assert!(
        edge.confidence >= min,
        "{file} {caller} -> {callee} binds below the import tiers: {edge:?}"
    );
    assert_ne!(edge.reason, NEAREST_COPY_REASON, "{edge:?}");
}

/// The index binds `caller`'s call of `callee` to `want` alone, at the
/// confidence today's reading gives: the no-scope guards.
fn assert_call_target(observed: &Observed, file: &str, caller: &str, callee: &str, want: &str) {
    let found = calls(observed, file, caller, callee);
    let targets: Vec<&str> = found.iter().map(|e| e.target_file.as_str()).collect();
    assert_eq!(targets, [want], "{file} {caller} -> {callee}: {found:?}");
}

/// The index's `Imports` edge from `file` to `name` lands in `want`, and no
/// `Imports` edge from `file` lands in a `decoys` file.
fn assert_imports(observed: &Observed, file: &str, name: &str, want: &str, decoys: &[&str]) {
    let from_file: Vec<Bound> = index_edges(observed, RelType::Imports)
        .into_iter()
        .filter(|e| e.source_file == file)
        .collect();
    assert!(
        from_file
            .iter()
            .any(|e| e.target == name && e.target_file == want),
        "{file} imports {name} from {want}: {from_file:?}"
    );
    for decoy in decoys {
        assert!(
            from_file.iter().all(|e| e.target_file != *decoy),
            "{file} imports from {decoy}: {from_file:?}"
        );
    }
}

/// The overlay's `Calls` and `Imports` edges equal the index's, and the
/// reader served every fragment.
fn assert_overlay_parity(observed: &Observed) {
    let differing: Vec<String> = observed
        .lines()
        .into_iter()
        .filter(|line| {
            line.starts_with("input ")
                || (line.starts_with("edge ")
                    && (line.contains(" Calls ") || line.contains(" Imports ")))
        })
        .collect();
    assert!(
        differing.is_empty(),
        "overlay differs from the index:\n{}",
        differing.join("\n")
    );
}

// ─── Node: TypeScript / JavaScript ─────────────────────────────────────────

const AT_ALIAS: &str = r#"{ "compilerOptions": { "paths": { "@/*": ["src/*"] } } }"#;

/// Two packages, each with its own `@/*` → `src/*` and its own `util`.
/// `config` is `tsconfig.json` or `jsconfig.json`.
fn two_at_packages(ext: &str, config: &'static str) -> Vec<(String, &'static str)> {
    let mut files = Vec::new();
    for (package, caller) in [("a", "runA"), ("b", "runB")] {
        files.push((format!("packages/{package}/{config}"), AT_ALIAS));
        files.push((
            format!("packages/{package}/src/util.{ext}"),
            "export function helper() {\n  return 1;\n}\n",
        ));
        let main: &'static str = match caller {
            "runA" => "import { helper } from \"@/util\";\n\nexport function runA() {\n  return helper();\n}\n",
            _ => "import { helper } from \"@/util\";\n\nexport function runB() {\n  return helper();\n}\n",
        };
        files.push((format!("packages/{package}/src/main.{ext}"), main));
    }
    files
}

fn borrowed<'a>(files: &'a [(String, &'static str)]) -> Vec<(&'a str, &'static str)> {
    files
        .iter()
        .map(|(path, source)| (path.as_str(), *source))
        .collect()
}

/// Both callers bind their own package's `helper` through `@/util` at the
/// named-import tier, with an `Imports` edge, and never the root `src/util`.
fn check_two_at_packages(name: &str, files: &[(String, &'static str)], ext: &str, decoy: &[&str]) {
    let files = borrowed(files);
    let main_a = format!("packages/a/src/main.{ext}");
    let main_b = format!("packages/b/src/main.{ext}");
    let util_a = format!("packages/a/src/util.{ext}");
    let util_b = format!("packages/b/src/util.{ext}");
    let observed = observe(name, &files, &main_a);
    assert_scoped_call(&observed, &main_a, "runA", "helper", &util_a, IMPORT_SCOPED);
    assert_scoped_call(&observed, &main_b, "runB", "helper", &util_b, IMPORT_SCOPED);
    let mut decoys_a: Vec<&str> = decoy.to_vec();
    decoys_a.push(&util_b);
    assert_imports(&observed, &main_a, "helper", &util_a, &decoys_a);
    let mut decoys_b: Vec<&str> = decoy.to_vec();
    decoys_b.push(&util_a);
    assert_imports(&observed, &main_b, "helper", &util_b, &decoys_b);
    assert_overlay_parity(&observed);
}

/// The spec repro (DIR area 4): with no root config, ecp 0.16.4 writes no
/// `Imports` edge and binds each call by path proximity at 0.6. Red on HEAD:
/// the confidence and the `Imports` edge.
#[test]
fn test_resolve_import_monorepo_two_at_aliases_bind_own_package() {
    check_two_at_packages(
        "workspace_scope_ts_two_at_aliases",
        &two_at_packages("ts", "tsconfig.json"),
        "ts",
        &[],
    );
}

/// The spec repro with a root `tsconfig.json` mapping `@/*` and a root
/// `src/util.ts`: ecp 0.16.4 binds both calls to the root file at 0.95. Red
/// on HEAD: the target file.
#[test]
fn test_resolve_import_monorepo_two_at_aliases_root_config_bind_own_package() {
    let mut files = two_at_packages("ts", "tsconfig.json");
    files.push(("tsconfig.json".to_string(), AT_ALIAS));
    files.push((
        "src/util.ts".to_string(),
        "export function helper() {\n  return 0;\n}\n",
    ));
    check_two_at_packages(
        "workspace_scope_ts_two_at_aliases_root",
        &files,
        "ts",
        &["src/util.ts"],
    );
}

/// A Vue `<script setup lang="ts">` importer is TypeScript by its stored
/// language, not by its path: it reads `packages/web`'s `@/*`, not the root's.
/// Red on HEAD: the scope lookup read the `.vue` path's language (none), so
/// the call bound the root `packages/shared/src/util.ts`.
#[test]
fn test_resolve_import_vue_script_importer_binds_own_package() {
    let files = [
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@/*": ["packages/shared/src/*"] } } }"#,
        ),
        ("packages/web/tsconfig.json", AT_ALIAS),
        (
            "packages/shared/src/util.ts",
            "export function helper() {\n  return 0;\n}\n",
        ),
        (
            "packages/web/src/util.ts",
            "export function helper() {\n  return 1;\n}\n",
        ),
        (
            "packages/web/src/App.vue",
            "<script setup lang=\"ts\">\nimport { helper } from \"@/util\";\n\nexport function run() {\n  return helper();\n}\n</script>\n",
        ),
    ];
    let main = "packages/web/src/App.vue";
    let util = "packages/web/src/util.ts";
    let observed = observe("workspace_scope_vue_script_importer", &files, main);
    assert_scoped_call(&observed, main, "run", "helper", util, IMPORT_SCOPED);
    assert_imports(
        &observed,
        main,
        "helper",
        util,
        &["packages/shared/src/util.ts"],
    );
    assert_overlay_parity(&observed);
}

/// JavaScript reads `jsconfig.json` through the same Node scope rule. Red on
/// HEAD as the TypeScript repro.
#[test]
fn test_resolve_import_monorepo_two_jsconfig_at_aliases_bind_own_package() {
    check_two_at_packages(
        "workspace_scope_js_two_at_aliases",
        &two_at_packages("js", "jsconfig.json"),
        "js",
        &[],
    );
}

const WORKSPACE_ROOT: &str = r#"{ "private": true, "workspaces": ["packages/*"] }"#;
const DEPENDS_ON_B: &str =
    r#"{ "name": "@scope/a", "dependencies": { "@scope/b": "workspace:*" } }"#;
const CALLS_F: &str =
    "import { f } from \"@scope/b\";\n\nexport function run() {\n  return f();\n}\n";
const EXPORTS_F: &str = "export function f() {\n  return 1;\n}\n";

/// `@scope/b` names the workspace package whose `package.json` `name` it is,
/// and its `main` entry holds `f`. The namesake `@scope/c` exports `f` too.
/// Red on HEAD: no alias names the package, so `f` has two candidates.
#[test]
fn test_resolve_import_workspace_package_name_binds_package_entry() {
    let files = [
        ("package.json", WORKSPACE_ROOT),
        ("packages/a/package.json", DEPENDS_ON_B),
        ("packages/a/src/main.ts", CALLS_F),
        (
            "packages/b/package.json",
            r#"{ "name": "@scope/b", "main": "src/index.ts" }"#,
        ),
        ("packages/b/src/index.ts", EXPORTS_F),
        (
            "packages/c/package.json",
            r#"{ "name": "@scope/c", "main": "src/index.ts" }"#,
        ),
        ("packages/c/src/index.ts", EXPORTS_F),
    ];
    let main = "packages/a/src/main.ts";
    let observed = observe("workspace_scope_ts_package_name", &files, main);
    assert_scoped_call(
        &observed,
        main,
        "run",
        "f",
        "packages/b/src/index.ts",
        IMPORT_SCOPED,
    );
    assert_imports(
        &observed,
        main,
        "f",
        "packages/b/src/index.ts",
        &["packages/c/src/index.ts"],
    );
    assert_overlay_parity(&observed);
}

/// Two workspace packages carry the name `@scope/b`: the dependency names
/// neither (C2 `RequirementTarget::ByName`), so no scope entry binds it and
/// the call keeps today's reading, which binds neither at an import tier.
/// Expected green on HEAD.
#[test]
fn test_resolve_import_two_scopes_one_package_name_binds_no_scope_entry() {
    let files = [
        ("package.json", WORKSPACE_ROOT),
        ("packages/a/package.json", DEPENDS_ON_B),
        ("packages/a/src/main.ts", CALLS_F),
        (
            "packages/b/package.json",
            r#"{ "name": "@scope/b", "main": "src/index.ts" }"#,
        ),
        ("packages/b/src/index.ts", EXPORTS_F),
        (
            "packages/b2/package.json",
            r#"{ "name": "@scope/b", "main": "src/index.ts" }"#,
        ),
        ("packages/b2/src/index.ts", EXPORTS_F),
    ];
    let main = "packages/a/src/main.ts";
    let observed = observe("workspace_scope_ts_one_name_two_scopes", &files, main);
    let found = calls(&observed, main, "run", "f");
    assert!(
        found.iter().all(|e| e.confidence < SCOPED),
        "no scope entry binds an ambiguous package name: {found:?}"
    );
    assert_overlay_parity(&observed);
}

/// A scope alias may name a file outside the scope (`../shared`): the
/// replacement is repo-relative, so it binds there, as `tsc` resolves it.
/// Red on HEAD: no alias, and two `log` namesakes.
#[test]
fn test_resolve_import_alias_outside_scope_binds_named_file() {
    let files = [
        (
            "packages/a/tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@shared/*": ["../shared/src/*"] } } }"#,
        ),
        (
            "packages/a/src/main.ts",
            "import { log } from \"@shared/log\";\n\nexport function run() {\n  return log();\n}\n",
        ),
        (
            "packages/shared/src/log.ts",
            "export function log() {\n  return 1;\n}\n",
        ),
        (
            "packages/b/src/log.ts",
            "export function log() {\n  return 2;\n}\n",
        ),
    ];
    let main = "packages/a/src/main.ts";
    let observed = observe("workspace_scope_ts_alias_outside", &files, main);
    assert_scoped_call(
        &observed,
        main,
        "run",
        "log",
        "packages/shared/src/log.ts",
        IMPORT_SCOPED,
    );
    assert_imports(
        &observed,
        main,
        "log",
        "packages/shared/src/log.ts",
        &["packages/b/src/log.ts"],
    );
    assert_overlay_parity(&observed);
}

/// A package `tsconfig.json` that does not parse leaves its scope with no
/// alias (R-A4a keeps the row): the repo-wide reading applies, so the call
/// binds the root `src/util.ts` as today. Expected green on HEAD.
#[test]
fn test_resolve_import_invalid_scope_config_keeps_repo_wide_reading() {
    let files = [
        ("tsconfig.json", AT_ALIAS),
        ("src/util.ts", "export function helper() {\n  return 0;\n}\n"),
        ("packages/a/tsconfig.json", "{ \"compilerOptions\": "),
        ("packages/a/src/util.ts", "export function helper() {\n  return 1;\n}\n"),
        (
            "packages/a/src/main.ts",
            "import { helper } from \"@/util\";\n\nexport function run() {\n  return helper();\n}\n",
        ),
    ];
    let main = "packages/a/src/main.ts";
    let observed = observe("workspace_scope_ts_invalid_config", &files, main);
    assert_scoped_call(
        &observed,
        main,
        "run",
        "helper",
        "src/util.ts",
        IMPORT_SCOPED,
    );
    assert_overlay_parity(&observed);
}

/// No config anywhere: the file is in no scope and a relative import binds
/// as today. Expected green on HEAD.
#[test]
fn test_resolve_import_ts_no_scope_keeps_relative_edge() {
    let files = [
        ("src/util.ts", "export function helper() {\n  return 1;\n}\n"),
        ("lib/util.ts", "export function helper() {\n  return 2;\n}\n"),
        (
            "src/main.ts",
            "import { helper } from \"./util\";\n\nexport function run() {\n  return helper();\n}\n",
        ),
    ];
    let observed = observe("workspace_scope_ts_no_scope", &files, "src/main.ts");
    assert_call_target(&observed, "src/main.ts", "run", "helper", "src/util.ts");
    assert_imports(
        &observed,
        "src/main.ts",
        "helper",
        "src/util.ts",
        &["lib/util.ts"],
    );
    assert_overlay_parity(&observed);
}

// ─── Go ─────────────────────────────────────────────────────────────────────

const GO_HELPER: &str = "package util\n\nfunc Helper() {}\n";

/// The inner `go.mod` declares its own module path: `example.com/inner/util`
/// is `inner/util`, not the root module's `util`. Red on HEAD: the root
/// module's alias does not match the inner path, so `Helper` has two
/// candidates.
#[test]
fn test_resolve_import_go_nested_module_binds_inner_module() {
    let main = "inner/cmd/main.go";
    let files = [
        ("go.mod", "module example.com/root\n\ngo 1.21\n"),
        ("util/util.go", GO_HELPER),
        ("inner/go.mod", "module example.com/inner\n\ngo 1.21\n"),
        ("inner/util/util.go", GO_HELPER),
        (
            main,
            "package main\n\nimport \"example.com/inner/util\"\n\nfunc run() {\n\tutil.Helper()\n}\n",
        ),
    ];
    let observed = observe("workspace_scope_go_nested_module", &files, main);
    assert_scoped_call(
        &observed,
        main,
        "run",
        "Helper",
        "inner/util/util.go",
        SCOPED,
    );
    assert_overlay_parity(&observed);
}

/// No `go.mod`: no scope row, so the scope reading never binds the call to the
/// other package's namesake. Today a same-package call with a namesake in
/// another package binds nothing at all, with or without `go.mod` (measured on
/// 0.16.4 and on the commit before this slice); that gap is
/// FU-2026-10-26-e29a865104d9, so this guard asserts only the decoy and
/// parity, not the missing edge.
#[test]
fn test_resolve_import_go_no_scope_keeps_package_edge() {
    let files = [
        ("pkg/a.go", "package pkg\n\nfunc Helper() {}\n"),
        ("pkg/b.go", "package pkg\n\nfunc run() {\n\tHelper()\n}\n"),
        ("other/c.go", "package other\n\nfunc Helper() {}\n"),
    ];
    let observed = observe("workspace_scope_go_no_scope", &files, "pkg/b.go");
    let found = calls(&observed, "pkg/b.go", "run", "Helper");
    assert!(
        found.iter().all(|e| e.target_file != "other/c.go"),
        "binds the other package's namesake: {found:?}"
    );
    assert_overlay_parity(&observed);
}

// ─── Python ─────────────────────────────────────────────────────────────────

/// A nested `pyproject.toml` whose project name equals its flat-layout
/// package (`flaskr`, as in the Flask tutorial) maps `flaskr/*` to
/// `src/flaskr/*` first. Red before the fix: Python discovery probed only that
/// first base, so `from flaskr.db import get_db` missed the import tier, and
/// with a namesake elsewhere the call bound nothing at all.
#[test]
fn test_resolve_import_python_project_name_equals_flat_package_binds_module() {
    let files = [
        ("pyproject.toml", "[project]\nname = \"outer\"\n"),
        ("ex/tut/pyproject.toml", "[project]\nname = \"flaskr\"\n"),
        ("ex/tut/flaskr/__init__.py", ""),
        ("ex/tut/flaskr/db.py", "def get_db():\n    return 1\n"),
        (
            "ex/tut/tests/test_db.py",
            "from flaskr.db import get_db\n\n\ndef test_get():\n    return get_db()\n",
        ),
        ("other/db.py", "def get_db():\n    return 2\n"),
    ];
    let observed = observe(
        "workspace_scope_python_project_name_flat",
        &files,
        "ex/tut/tests/test_db.py",
    );
    assert_scoped_call(
        &observed,
        "ex/tut/tests/test_db.py",
        "test_get",
        "get_db",
        "ex/tut/flaskr/db.py",
        SCOPED,
    );
    assert_overlay_parity(&observed);
}

/// Two `pyproject.toml` projects each hold a package `app`: `app.models`
/// from `svc_a` is `svc_a/app/models.py`. Red on HEAD: two source roots make
/// the module ambiguous.
#[test]
fn test_resolve_import_python_package_root_binds_scope_root() {
    let mut files: Vec<(String, &'static str)> = Vec::new();
    for (project, name) in [("svc_a", "svc-a"), ("svc_b", "svc-b")] {
        let pyproject: &'static str = match name {
            "svc-a" => "[project]\nname = \"svc-a\"\n",
            _ => "[project]\nname = \"svc-b\"\n",
        };
        files.push((format!("{project}/pyproject.toml"), pyproject));
        files.push((format!("{project}/app/__init__.py"), ""));
        files.push((
            format!("{project}/app/models.py"),
            "def helper():\n    return 1\n",
        ));
        files.push((
            format!("{project}/app/main.py"),
            "from app.models import helper\n\n\ndef run():\n    return helper()\n",
        ));
    }
    let files = borrowed(&files);
    let observed = observe(
        "workspace_scope_python_two_projects",
        &files,
        "svc_a/app/main.py",
    );
    for project in ["svc_a", "svc_b"] {
        assert_scoped_call(
            &observed,
            &format!("{project}/app/main.py"),
            "run",
            "helper",
            &format!("{project}/app/models.py"),
            SCOPED,
        );
    }
    assert_overlay_parity(&observed);
}

/// No `pyproject.toml`: one package `app` binds as today. Expected green on
/// HEAD.
#[test]
fn test_resolve_import_python_no_scope_keeps_module_edge() {
    let files = [
        ("app/__init__.py", ""),
        ("app/models.py", "def helper():\n    return 1\n"),
        ("lib/models.py", "def helper():\n    return 2\n"),
        (
            "app/main.py",
            "from app.models import helper\n\n\ndef run():\n    return helper()\n",
        ),
    ];
    let observed = observe("workspace_scope_python_no_scope", &files, "app/main.py");
    assert_call_target(&observed, "app/main.py", "run", "helper", "app/models.py");
    assert_overlay_parity(&observed);
}

// ─── PHP ────────────────────────────────────────────────────────────────────

fn php_util(namespace: &str) -> String {
    format!(
        "<?php\nnamespace {namespace};\n\nclass Util\n{{\n    public static function helper()\n    {{\n    }}\n}}\n"
    )
}

fn php_main(namespace: &str) -> String {
    format!(
        "<?php\nnamespace {namespace}\\Entry;\n\nuse {namespace}\\Util;\n\nfunction run()\n{{\n    Util::helper();\n}}\n"
    )
}

/// Two Composer packages with their own PSR-4 prefixes: each `Util` is in
/// its own namespace, so each call binds its own package. Expected green on
/// HEAD: PHP binds through the declared namespace, which the prefixes do not
/// change.
#[test]
fn test_resolve_import_php_composer_psr4_binds_own_package() {
    let util_a = php_util("A");
    let util_b = php_util("B");
    let main_a = php_main("A");
    let main_b = php_main("B");
    let files = [
        (
            "pkg_a/composer.json",
            r#"{ "name": "acme/a", "autoload": { "psr-4": { "A\\": "src/" } } }"#,
        ),
        ("pkg_a/src/Util.php", util_a.as_str()),
        ("pkg_a/src/Main.php", main_a.as_str()),
        (
            "pkg_b/composer.json",
            r#"{ "name": "acme/b", "autoload": { "psr-4": { "B\\": "src/" } } }"#,
        ),
        ("pkg_b/src/Util.php", util_b.as_str()),
        ("pkg_b/src/Main.php", main_b.as_str()),
    ];
    let observed = observe(
        "workspace_scope_php_two_prefixes",
        &files,
        "pkg_a/src/Main.php",
    );
    for package in ["pkg_a", "pkg_b"] {
        assert_scoped_call(
            &observed,
            &format!("{package}/src/Main.php"),
            "run",
            "helper",
            &format!("{package}/src/Util.php"),
            SCOPED,
        );
    }
    assert_overlay_parity(&observed);
}

/// Two Composer packages share the prefix `App\`, and each declares
/// `App\Util`: the caller's package root picks its own. Red on HEAD: both
/// files declare the imported member, so the namespace answer has no unique
/// hit.
#[test]
fn test_resolve_import_php_composer_same_prefix_binds_own_package() {
    let util = php_util("App");
    let main = php_main("App");
    let composer = r#"{ "name": "acme/NAME", "autoload": { "psr-4": { "App\\": "src/" } } }"#;
    let composer_a = composer.replace("NAME", "a");
    let composer_b = composer.replace("NAME", "b");
    let files = [
        ("pkg_a/composer.json", composer_a.as_str()),
        ("pkg_a/src/Util.php", util.as_str()),
        ("pkg_a/src/Main.php", main.as_str()),
        ("pkg_b/composer.json", composer_b.as_str()),
        ("pkg_b/src/Util.php", util.as_str()),
        ("pkg_b/src/Main.php", main.as_str()),
    ];
    let observed = observe(
        "workspace_scope_php_one_prefix",
        &files,
        "pkg_a/src/Main.php",
    );
    for package in ["pkg_a", "pkg_b"] {
        assert_scoped_call(
            &observed,
            &format!("{package}/src/Main.php"),
            "run",
            "helper",
            &format!("{package}/src/Util.php"),
            SCOPED,
        );
    }
    assert_overlay_parity(&observed);
}

/// No `composer.json`: the namespace import binds as today. Expected green
/// on HEAD.
#[test]
fn test_resolve_import_php_no_scope_keeps_namespace_edge() {
    let util = php_util("App");
    let other = php_util("Other");
    let main = php_main("App");
    let files = [
        ("src/Util.php", util.as_str()),
        ("lib/Util.php", other.as_str()),
        ("src/Main.php", main.as_str()),
    ];
    let observed = observe("workspace_scope_php_no_scope", &files, "src/Main.php");
    assert_call_target(&observed, "src/Main.php", "run", "helper", "src/Util.php");
    assert_overlay_parity(&observed);
}

// ─── C# ─────────────────────────────────────────────────────────────────────

const CS_LIB: &str =
    "namespace Shared\n{\n    public static class Lib\n    {\n        public static void Helper() {}\n    }\n}\n";
const CS_PROGRAM: &str = "using Shared;\n\nnamespace App\n{\n    class Program\n    {\n        static void Run()\n        {\n            Lib.Helper();\n        }\n    }\n}\n";

/// A root `App.csproj` references `Shared/Shared.csproj`. The project scope
/// declares no alias, so today's repo-wide `ProjectReference` alias stays in
/// its reading (`path_aliases.rs`
/// `test_expand_in_dotnet_project_keeps_repo_wide_project_reference_alias`),
/// and the call into the referenced project binds as today. Expected green on
/// HEAD.
#[test]
fn test_resolve_import_csharp_project_binds_own_project() {
    let files = [
        (
            "App.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <ProjectReference Include=\"Shared/Shared.csproj\" />\n  </ItemGroup>\n</Project>\n",
        ),
        ("Shared/Shared.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\" />\n"),
        ("Shared/Lib.cs", CS_LIB),
        ("Program.cs", CS_PROGRAM),
    ];
    let observed = observe(
        "workspace_scope_csharp_project_reference",
        &files,
        "Program.cs",
    );
    assert_call_target(&observed, "Program.cs", "Run", "Helper", "Shared/Lib.cs");
    assert_overlay_parity(&observed);
}

/// Two projects each declare `Shared.Lib`; the call in project `a` should
/// bind `a`'s. Not delivered by R-A4b: a C# call binds through the type
/// ladder's global candidates (`Resolver::type_candidates`), which read no
/// import candidate and no module discovery, so the scope reading cannot
/// reach it. Run with `--ignored` to see today's answer.
#[test]
#[ignore = "R-A4b: C# binds through the type ladder's global candidates, which no scope reading reaches"]
fn test_resolve_import_csharp_two_projects_one_namespace_binds_own_project() {
    let files = [
        ("a/A.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\" />\n"),
        ("a/Lib.cs", CS_LIB),
        ("a/Program.cs", CS_PROGRAM),
        ("b/B.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\" />\n"),
        ("b/Lib.cs", CS_LIB),
    ];
    let observed = observe(
        "workspace_scope_csharp_two_projects",
        &files,
        "a/Program.cs",
    );
    assert_scoped_call(
        &observed,
        "a/Program.cs",
        "Run",
        "Helper",
        "a/Lib.cs",
        SCOPED,
    );
    assert_overlay_parity(&observed);
}

/// No `.csproj`: the namespace import binds as today. Expected green on HEAD.
#[test]
fn test_resolve_import_csharp_no_scope_keeps_namespace_edge() {
    let files = [("Shared/Lib.cs", CS_LIB), ("Program.cs", CS_PROGRAM)];
    let observed = observe("workspace_scope_csharp_no_scope", &files, "Program.cs");
    assert_call_target(&observed, "Program.cs", "Run", "Helper", "Shared/Lib.cs");
    assert_overlay_parity(&observed);
}
