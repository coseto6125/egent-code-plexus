//! A qualified function passed as a value from a dirty file (Rust
//! `parse::f`, TypeScript `ns.f`, Go `pkg.F`, C++ `ns::f`): the session
//! overlay emits the same `References` edge (reason `function_reference`)
//! as a full reindex of the edited tree, reason and confidence included,
//! and masks the base edge a dirty file no longer makes.
//!
//! Each fixture keeps the function in a clean file and its user in the
//! dirty one, and runs two edits:
//!
//! | edit             | the reindex has                         |
//! |------------------|-----------------------------------------|
//! | add the value    | the user's edge to the qualified function |
//! | remove the value | none; the stale base edge must go        |

use crate::parity;

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::path_aliases::PathAliases;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::RelType;
use parity::{edges_after_edit_with, EdgeParity};
use std::path::Path;

const FUNCTION_REFERENCE: &str = "function_reference";

struct LangFixture {
    lang: &'static str,
    /// The clean files, the function's own among them.
    files: &'static [(&'static str, &'static str)],
    /// The dirty file's path, its source without the value, and with it.
    user: (&'static str, &'static str, &'static str),
    /// `(source function, target file, target)` the reindex emits from
    /// the user with the value.
    row: (&'static str, &'static str, &'static str),
}

const LANGS: &[LangFixture] = &[
    LangFixture {
        lang: "Rust",
        files: &[
            ("src/lib.rs", "pub mod parse;\npub mod cache;\n"),
            ("src/parse.rs", "pub fn migrate(_: &mut u32) {}\n"),
        ],
        user: (
            "src/cache.rs",
            "pub fn all(v: &mut Vec<u32>) {\n    let _ = v;\n}\n",
            "pub fn all(v: &mut Vec<u32>) {\n    v.iter_mut().for_each(crate::parse::migrate);\n}\n",
        ),
        row: ("all", "src/parse.rs", "migrate"),
    },
    LangFixture {
        lang: "TypeScript",
        files: &[(
            "lib.ts",
            "export function f(x: number): number {\n  return x;\n}\n",
        )],
        user: (
            "app.ts",
            "import * as ns from './lib';\nexport function run(items: number[]): number[] {\n  return items;\n}\n",
            "import * as ns from './lib';\nexport function run(items: number[]): number[] {\n  return items.map(ns.f);\n}\n",
        ),
        row: ("run", "lib.ts", "f"),
    },
    LangFixture {
        lang: "Go",
        files: &[("pkg/helper.go", "package pkg\n\nfunc F() {}\n")],
        user: (
            "cmd/app.go",
            "package main\n\nimport \"example.com/app/pkg\"\n\nfunc register(f func()) {}\n\nfunc boot() {\n\tregister(nil)\n}\n",
            "package main\n\nimport \"example.com/app/pkg\"\n\nfunc register(f func()) {}\n\nfunc boot() {\n\tregister(pkg.F)\n}\n",
        ),
        row: ("boot", "pkg/helper.go", "F"),
    },
    LangFixture {
        lang: "C++",
        files: &[("ns.cpp", "namespace ns {\nint f(int x) { return x; }\n}\n")],
        user: (
            "use.cpp",
            "#include <algorithm>\n#include <vector>\nvoid apply(std::vector<int>& xs) {\n    xs.clear();\n}\n",
            "#include <algorithm>\n#include <vector>\nvoid apply(std::vector<int>& xs) {\n    std::for_each(xs.begin(), xs.end(), ns::f);\n}\n",
        ),
        row: ("apply", "ns.cpp", "f"),
    },
];

fn parse(path: &str, source: &str) -> LocalGraph {
    ecp_cli::reanalyze::pipeline()
        .parse_file_raw(Path::new(path), source.as_bytes())
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The builder of `lang`'s fixture. Go gets a module at `root` with the
/// alias the CLI derives from its `go.mod` (`example.com/app/*` -> `./*`)
/// and the files of `tree` on disk; every other language needs neither.
fn builder_for(lang: &str, root: &Path, tree: &[(&str, &str)]) -> Box<dyn Fn() -> GraphBuilder> {
    if lang != "Go" {
        return Box::new(GraphBuilder::new);
    }
    std::fs::write(root.join("go.mod"), "module example.com/app\n\ngo 1.21\n").unwrap();
    for (path, source) in tree {
        let disk = root.join(path);
        std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
        std::fs::write(disk, source).unwrap();
    }
    let mut aliases = PathAliases::new();
    aliases.add("example.com/app/*", vec!["./*".to_string()]);
    let root = root.to_path_buf();
    Box::new(move || {
        GraphBuilder::new()
            .with_repo_root(root.clone())
            .with_path_aliases(aliases.clone())
    })
}

/// Every mismatch of the two edits of `fixture`, one line each: a reindex
/// that differs from the expected rows (the fixture would prove nothing),
/// or an overlay that differs from the reindex in any field.
fn mismatches(fixture: &LangFixture) -> Vec<String> {
    let (user_path, bare, with_value) = fixture.user;
    let mut failures = Vec::new();
    for (edit, base, edited, expected) in [
        ("add the value", bare, with_value, vec![fixture.row]),
        ("remove the value", with_value, bare, Vec::new()),
    ] {
        let mut files = fixture.files.to_vec();
        files.push((user_path, base));
        let tmp = tempfile::tempdir().unwrap();
        let mut tree = fixture.files.to_vec();
        tree.push((user_path, edited));
        let new_builder = builder_for(fixture.lang, tmp.path(), &tree);
        let EdgeParity { reindex, overlay } = edges_after_edit_with(
            new_builder.as_ref(),
            &parse,
            &files,
            &[(user_path, edited)],
            RelType::References,
            &|file, _| file == user_path,
        );
        let only = |edges: Vec<parity::ReasonedEdge>| -> Vec<parity::ReasonedEdge> {
            edges
                .into_iter()
                .filter(|e| e.reason == FUNCTION_REFERENCE)
                .collect()
        };
        let (reindex, overlay) = (only(reindex), only(overlay));
        let got: Vec<(&str, &str, &str)> = reindex
            .iter()
            .map(|e| {
                (
                    e.edge.caller.as_str(),
                    e.edge.callee_file.as_str(),
                    e.edge.callee.as_str(),
                )
            })
            .collect();
        let label = format!("{}/{edit}", fixture.lang);
        if got != expected {
            failures.push(format!(
                "{label}: the reindex emits {got:?}, the fixture expects {expected:?}"
            ));
            continue;
        }
        if overlay != reindex {
            failures.push(format!(
                "{label}: overlay {overlay:#?}\nreindex {reindex:#?}"
            ));
        }
    }
    failures
}

#[test]
fn test_overlay_qualified_function_reference_matches_reindex() {
    let failures: Vec<String> = LANGS.iter().flat_map(mismatches).collect();
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
