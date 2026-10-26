//! `Imports` edges of the incremental overlay (F7b1-2): for a dirty file,
//! the overlay view rebuilds the `Imports` edges from its `File` node as a
//! fresh full index of the edited tree has them, with the index's
//! confidence and reason, in each of the 14 mainstream languages, through
//! the production index, fragment write and read (`common/strict_parity.rs`).
//!
//! Each check first asserts that the full index imports the modules the
//! edit adds and that the base index imported the ones it drops, so a
//! fixture whose parser emits no `Imports` edge fails instead of passing
//! empty, and the overlay cannot pass by keeping the base index's edges.
//!
//! Every one of the 14 languages emits `Imports` edges
//! (`crates/ecp-cli/tests/imports_edge_inspect.rs` has one per language),
//! so this file has no exemption line. Only the edges of the dirty file are
//! checked: a clean importer of a changed export keeps its indexed edges
//! (F7b2).

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind, RelType};
use ecp_core::session::merged::MergedGraph;
use strict_parity::{Edit, Scenario};

const OWNER: &str = "lane F (incremental overlay: builder edges of dirty files)";

/// One language's importer and its modules. `module(name)` is the path and
/// source of the module that declares `name`, `import(name)` the line that
/// imports it, and `body(imports)` the importer's source around its import
/// lines.
struct Fixture {
    lang: &'static str,
    extra: &'static [(&'static str, &'static str)],
    main: &'static str,
    module: fn(&str) -> (String, String),
    import: fn(&str) -> String,
    body: fn(&str) -> String,
}

/// The module names: the importer imports `Alpha`, `Beta`, `Epsilon`
/// before the edit and `Gamma`, `Beta`, `Delta` after it.
const NAMES: [&str; 5] = ["Alpha", "Beta", "Gamma", "Delta", "Epsilon"];
const BEFORE: [&str; 3] = ["Alpha", "Beta", "Epsilon"];
const AFTER: [&str; 3] = ["Gamma", "Beta", "Delta"];

/// Syntax per language as `imports_edge_inspect.rs` has it. TypeScript,
/// JavaScript, Python, Java, Kotlin, PHP and Rust bind the imported
/// declaration (step 1); Ruby, Dart, Go, C, C++, C# and Swift bind the
/// module's `File` (step 3).
fn fixtures() -> [Fixture; 14] {
    [
    Fixture {
        lang: "typescript",
        extra: &[],
        main: "src/main.ts",
        module: |n| {
            (
                format!("src/{}.ts", n.to_lowercase()),
                format!("export function {n}(): number {{\n  return 1;\n}}\n"),
            )
        },
        import: |n| format!("import {{ {n} }} from './{}';\n", n.to_lowercase()),
        body: |i| format!("{i}\nexport function run(): number {{\n  return 0;\n}}\n"),
    },
    Fixture {
        lang: "javascript",
        extra: &[],
        main: "src/main.mjs",
        module: |n| {
            (
                format!("src/{}.mjs", n.to_lowercase()),
                format!("export function {n}() {{\n  return 1;\n}}\n"),
            )
        },
        import: |n| format!("import {{ {n} }} from './{}.mjs';\n", n.to_lowercase()),
        body: |i| format!("{i}\nexport function run() {{\n  return 0;\n}}\n"),
    },
    Fixture {
        lang: "python",
        extra: &[],
        main: "main.py",
        module: |n| {
            (
                format!("{}.py", n.to_lowercase()),
                format!("def {n}():\n    return 1\n"),
            )
        },
        import: |n| format!("from {} import {n}\n", n.to_lowercase()),
        body: |i| format!("{i}\n\ndef run():\n    return 0\n"),
    },
    Fixture {
        lang: "java",
        extra: &[],
        main: "src/com/y/Main.java",
        module: |n| {
            (
                format!("src/com/x/{n}.java"),
                format!("package com.x;\npublic class {n} {{ public int bar() {{ return 1; }} }}\n"),
            )
        },
        import: |n| format!("import com.x.{n};\n"),
        body: |i| format!("package com.y;\n{i}public class Main {{ }}\n"),
    },
    Fixture {
        lang: "kotlin",
        extra: &[],
        main: "src/com/y/Main.kt",
        module: |n| {
            (
                format!("src/com/x/{n}.kt"),
                format!("package com.x\nclass {n} {{ fun bar() = 1 }}\n"),
            )
        },
        import: |n| format!("import com.x.{n}\n"),
        body: |i| format!("package com.y\n{i}class Main\n"),
    },
    Fixture {
        lang: "csharp",
        extra: &[],
        main: "src/App/Main.cs",
        module: |n| {
            (
                format!("src/{n}/{n}Type.cs"),
                format!("namespace {n} {{ public class {n}Type {{ public int Bar() => 1; }} }}\n"),
            )
        },
        import: |n| format!("using {n};\n"),
        body: |i| format!("{i}namespace App {{ public class Main {{ }} }}\n"),
    },
    Fixture {
        lang: "go",
        extra: &[("go.mod", "module sample\n\ngo 1.21\n")],
        main: "cmd/main.go",
        module: |n| {
            let l = n.to_lowercase();
            (
                format!("{l}/{l}.go"),
                format!("package {l}\n\nfunc {n}() int {{ return 1 }}\n"),
            )
        },
        import: |n| format!("import \"sample/{}\"\n", n.to_lowercase()),
        body: |i| format!("package main\n\n{i}\nfunc main() {{}}\n"),
    },
    Fixture {
        lang: "rust",
        extra: &[
            (
                "Cargo.toml",
                "[package]\nname = \"sample\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            (
                "src/lib.rs",
                "pub mod alpha;\npub mod beta;\npub mod gamma;\npub mod delta;\npub mod epsilon;\npub mod app;\n",
            ),
        ],
        main: "src/app.rs",
        module: |n| {
            let l = n.to_lowercase();
            (format!("src/{l}.rs"), format!("pub fn {l}() -> i32 {{ 1 }}\n"))
        },
        import: |n| {
            let l = n.to_lowercase();
            format!("use crate::{l}::{l};\n")
        },
        body: |i| format!("{i}pub fn run() -> i32 {{ 0 }}\n"),
    },
    Fixture {
        lang: "php",
        extra: &[],
        main: "src/Web/Main.php",
        module: |n| {
            (
                format!("src/App/{n}.php"),
                format!("<?php\nnamespace App;\nclass {n} {{ public function bar() {{ return 1; }} }}\n"),
            )
        },
        import: |n| format!("use App\\{n};\n"),
        body: |i| format!("<?php\nnamespace Web;\n{i}class Main {{ }}\n"),
    },
    Fixture {
        lang: "ruby",
        extra: &[],
        main: "lib/main.rb",
        module: |n| {
            (
                format!("lib/{}.rb", n.to_lowercase()),
                format!("class {n}\n  def bar\n    1\n  end\nend\n"),
            )
        },
        import: |n| format!("require_relative '{}'\n", n.to_lowercase()),
        body: |i| format!("{i}\nclass Main\nend\n"),
    },
    Fixture {
        lang: "swift",
        extra: &[],
        main: "Sources/App/Main.swift",
        module: |n| {
            (
                format!("Sources/{n}/{n}.swift"),
                format!("public struct {n} {{\n    public init() {{}}\n    public func bar() -> Int {{ 1 }}\n}}\n"),
            )
        },
        import: |n| format!("import {n}\n"),
        body: |i| format!("{i}\npublic struct Main {{\n    public init() {{}}\n}}\n"),
    },
    Fixture {
        lang: "c",
        extra: &[],
        main: "src/main.c",
        module: |n| {
            let l = n.to_lowercase();
            (format!("include/{l}.h"), format!("int {l}_value(void);\n"))
        },
        import: |n| format!("#include \"{}.h\"\n", n.to_lowercase()),
        body: |i| format!("{i}\nint run(void) {{ return 0; }}\n"),
    },
    Fixture {
        lang: "cpp",
        extra: &[],
        main: "src/main.cpp",
        module: |n| {
            (
                format!("include/{}.hpp", n.to_lowercase()),
                format!("#pragma once\nclass {n} {{ public: int bar(); }};\n"),
            )
        },
        import: |n| format!("#include \"{}.hpp\"\n", n.to_lowercase()),
        body: |i| format!("{i}\nint run() {{ return 0; }}\n"),
    },
    Fixture {
        lang: "dart",
        extra: &[],
        main: "lib/main.dart",
        module: |n| {
            (
                format!("lib/{}.dart", n.to_lowercase()),
                format!("class {n} {{\n  int bar() => 1;\n}}\n"),
            )
        },
        import: |n| format!("import '{}.dart';\n", n.to_lowercase()),
        body: |i| format!("{i}\nclass Main {{}}\n"),
    },
    ]
}

/// The `Imports` edges whose source is the `File` node of `path`, each as
/// its target, exact confidence and reason, sorted. `graph` resolves a base
/// edge's reason.
fn imports_from<'a>(
    merged: &MergedGraph<'a>,
    graph: &'a ArchivedZeroCopyGraph,
    path: &str,
) -> Vec<String> {
    let mut out: Vec<String> = merged
        .all_edges()
        .filter(|edge| edge.rel_type() == RelType::Imports)
        .filter(|edge| {
            merged.node(edge.source).is_some_and(|node| {
                node.kind() == NodeKind::File && node.file_path(merged) == Some(path)
            })
        })
        .filter_map(|edge| {
            let target = merged.node(edge.target)?;
            Some(format!(
                "{} {}:{} {} conf={:?} reason={}",
                target.kind().as_str(),
                target.file_path(merged).unwrap_or("-"),
                target.start_line(),
                target.name(merged),
                edge.confidence(),
                edge.reason(graph),
            ))
        })
        .collect();
    out.sort();
    out
}

fn names_file(lines: &[String], file: &str) -> bool {
    let at = format!(" {file}:");
    lines.iter().any(|line| line.contains(&at))
}

/// Run `files` + `edits` and assert the overlay's `Imports` edges of `path`
/// are the reindex's, that the reindex imports each of `gained`, and that
/// the base index imported each of `lost` and the reindex does not.
fn assert_imports_match(
    label: &str,
    files: &[(&str, &str)],
    edits: &[Edit<'_>],
    path: &str,
    gained: &[&str],
    lost: &[&str],
) {
    let observed = strict_parity::run(&Scenario {
        name: "",
        owner: OWNER,
        files,
        edits,
    });
    let (full_graph, base_graph) = (observed.full(), observed.base());
    let full = imports_from(&MergedGraph::new(full_graph, None), full_graph, path);
    for file in gained {
        assert!(
            names_file(&full, file),
            "{label}: the reindex imports {file}: {full:#?}"
        );
    }
    for file in lost {
        let base = imports_from(&MergedGraph::new(base_graph, None), base_graph, path);
        assert!(
            names_file(&base, file),
            "{label}: the base index imports {file}: {base:#?}"
        );
        assert!(
            !names_file(&full, file),
            "{label}: the reindex no longer imports {file}: {full:#?}"
        );
    }
    let view = observed.view();
    let overlay = imports_from(
        &MergedGraph::new(base_graph, view.as_ref()),
        base_graph,
        path,
    );
    assert_eq!(overlay, full, "{label}: the dirty file's Imports edges");
}

/// An edit that changes the first import to another module, keeps the
/// second, drops the third and adds a fourth: the overlay imports what a
/// reindex imports, with its confidence and reason, in each of the 14
/// languages. Red before F7b1-2: the overlay kept the base index's edges.
#[test]
fn test_overlay_view_dirty_imports_matches_reindex() {
    for fx in &fixtures() {
        let modules: Vec<(String, String)> = NAMES.iter().map(|n| (fx.module)(n)).collect();
        let main =
            |names: &[&str]| (fx.body)(&names.iter().map(|n| (fx.import)(n)).collect::<String>());
        let (before, after) = (main(BEFORE.as_slice()), main(AFTER.as_slice()));
        let mut files: Vec<(&str, &str)> = fx.extra.to_vec();
        files.extend(modules.iter().map(|(p, s)| (p.as_str(), s.as_str())));
        files.push((fx.main, before.as_str()));
        let path_of = |name: &str| {
            modules[NAMES.iter().position(|n| *n == name).unwrap()]
                .0
                .as_str()
        };
        assert_imports_match(
            fx.lang,
            &files,
            &[Edit::Write(fx.main, &after)],
            fx.main,
            &[path_of("Gamma"), path_of("Delta")],
            &[path_of("Alpha"), path_of("Epsilon")],
        );
    }
}

const TS_ALPHA: (&str, &str) = (
    "src/alpha.ts",
    "export function alpha(): number {\n  return 1;\n}\n",
);
const TS_BETA: (&str, &str) = (
    "src/beta.ts",
    "export function beta(): number {\n  return 2;\n}\n",
);

fn ts_main(imports: &str) -> String {
    format!("{imports}\nexport function run(): number {{\n  return 0;\n}}\n")
}

/// An import of a path no file has binds nothing on either side; the
/// import beside it binds as before.
#[test]
fn test_overlay_view_import_of_missing_path_matches_reindex() {
    let before = ts_main("import { alpha } from './alpha';\n");
    let after = ts_main("import { alpha } from './alpha';\nimport { gone } from './missing';\n");
    assert_imports_match(
        "import of a missing path",
        &[TS_ALPHA, ("src/main.ts", before.as_str())],
        &[Edit::Write("src/main.ts", &after)],
        "src/main.ts",
        &["src/alpha.ts"],
        &[],
    );
}

/// The import's target is a declaration the edit adds to another dirty
/// file: the overlay binds the virtual declaration a reindex binds.
#[test]
fn test_overlay_view_import_of_other_dirty_file_matches_reindex() {
    let before = ts_main("import { alpha } from './alpha';\n");
    let after = ts_main("import { alpha, fresh } from './alpha';\n");
    let alpha_after = "export function alpha(): number {\n  return 1;\n}\n\nexport function fresh(): number {\n  return 3;\n}\n";
    assert_imports_match(
        "import of another dirty file",
        &[TS_ALPHA, ("src/main.ts", before.as_str())],
        &[
            Edit::Write("src/alpha.ts", alpha_after),
            Edit::Write("src/main.ts", &after),
        ],
        "src/main.ts",
        &["src/alpha.ts"],
        &[],
    );
}

/// The edit removes the module an import names and touches the importer:
/// neither side binds into the removed file.
#[test]
fn test_overlay_view_import_of_removed_file_matches_reindex() {
    let imports = "import { alpha } from './alpha';\nimport { beta } from './beta';\n";
    let before = ts_main(imports);
    let after = format!("// edited\n{}", ts_main(imports));
    assert_imports_match(
        "import of a removed file",
        &[TS_ALPHA, TS_BETA, ("src/main.ts", before.as_str())],
        &[
            Edit::Delete("src/alpha.ts"),
            Edit::Write("src/main.ts", &after),
        ],
        "src/main.ts",
        &["src/beta.ts"],
        &["src/alpha.ts"],
    );
}

/// A file the edit empties imports nothing, as in a reindex.
#[test]
fn test_overlay_view_emptied_file_drops_its_imports() {
    let before = ts_main("import { alpha } from './alpha';\n");
    assert_imports_match(
        "emptied file",
        &[TS_ALPHA, ("src/main.ts", before.as_str())],
        &[Edit::Write("src/main.ts", "")],
        "src/main.ts",
        &[],
        &["src/alpha.ts"],
    );
}

/// A hit through a barrel's re-export keeps the importer's edge to the
/// barrel file (0.9, module reason) beside the declaration's.
#[test]
fn test_overlay_view_import_through_barrel_matches_reindex() {
    let before = ts_main("import { alpha } from './alpha';\n");
    let after = ts_main("import { beta } from './index';\n");
    assert_imports_match(
        "import through a barrel",
        &[
            TS_ALPHA,
            TS_BETA,
            ("src/index.ts", "export { beta } from './beta';\n"),
            ("src/main.ts", before.as_str()),
        ],
        &[Edit::Write("src/main.ts", &after)],
        "src/main.ts",
        &["src/beta.ts", "src/index.ts"],
        &["src/alpha.ts"],
    );
}

const AT_ALIAS: &str = r#"{ "compilerOptions": { "paths": { "@/*": ["src/*"] } } }"#;

/// R-A4b: two packages each map `@/*` to their own `src/*`. The dirty
/// importer of package `a` switches from `@/alpha` to `@/beta` and binds its
/// own package's `beta`, never package `b`'s.
#[test]
fn test_overlay_view_import_through_workspace_scoped_alias_matches_reindex() {
    let main =
        |module: &str, name: &str| ts_main(&format!("import {{ {name} }} from \"@/{module}\";\n"));
    let (before, after) = (main("alpha", "alpha"), main("beta", "beta"));
    let files = [
        ("packages/a/tsconfig.json", AT_ALIAS),
        ("packages/b/tsconfig.json", AT_ALIAS),
        ("packages/a/src/alpha.ts", TS_ALPHA.1),
        ("packages/a/src/beta.ts", TS_BETA.1),
        ("packages/b/src/alpha.ts", TS_ALPHA.1),
        ("packages/b/src/beta.ts", TS_BETA.1),
        ("packages/a/src/main.ts", before.as_str()),
    ];
    assert_imports_match(
        "workspace-scoped alias",
        &files,
        &[Edit::Write("packages/a/src/main.ts", &after)],
        "packages/a/src/main.ts",
        &["packages/a/src/beta.ts"],
        &["packages/a/src/alpha.ts"],
    );
}

fn vue_ts(imports: &str) -> String {
    format!("<script lang=\"ts\">\n{imports}export function setup() {{ return 1; }}\n</script>\n")
}

/// A `.vue` file with a TypeScript script imports a `.ts` module: the
/// overlay reads the script's language from the fragment, as the index
/// reads the stored one.
#[test]
fn test_overlay_view_vue_ts_script_import_matches_reindex() {
    let before = vue_ts("import { alpha } from './alpha';\n");
    let after = vue_ts("import { beta } from './beta';\n");
    assert_imports_match(
        "vue ts script",
        &[TS_ALPHA, TS_BETA, ("src/App.vue", before.as_str())],
        &[Edit::Write("src/App.vue", &after)],
        "src/App.vue",
        &["src/beta.ts"],
        &["src/alpha.ts"],
    );
}
