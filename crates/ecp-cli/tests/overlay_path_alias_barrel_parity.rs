//! P1: a clean TS barrel that re-exports through a tsconfig path alias
//! (`export * from '@/ui/button'`, `export { render } from '@/ui/button'`)
//! resolves in the session overlay as in a full reindex. `graph.bin` keeps
//! no path aliases, so before the fix the overlay probed the alias as a
//! literal path, found no module, and the call fell to the global tier (no
//! edge here, where a namesake makes the name ambiguous) while the reindex
//! bound it at 0.95. The archive now keeps the file each re-export row
//! resolved to (`ImportFact::target_file`).

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::resolution::path_aliases::PathAliases;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType};
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::view::{OverlayFileInput, OverlayView};
use std::path::Path;

/// `(callee file, callee name, callee line, confidence)` of `go`'s calls.
fn calls_of_go(merged: &MergedGraph<'_>) -> Vec<(String, String, u32, f32)> {
    let mut calls: Vec<_> = merged
        .all_edges()
        .filter(|edge| edge.rel_type() == RelType::Calls)
        .filter_map(|edge| {
            let caller = merged.node(edge.source)?;
            let callee = merged.node(edge.target)?;
            (caller.name(merged) == "go").then(|| {
                (
                    callee.file_path(merged).unwrap_or_default().to_string(),
                    callee.name(merged).to_string(),
                    callee.start_line(),
                    edge.confidence(),
                )
            })
        })
        .collect();
    calls.sort_by(|a, b| a.partial_cmp(b).expect("a confidence is never NaN"));
    calls.dedup();
    calls
}

fn build(provider: &TypeScriptProvider, tree: &[(&str, &str)]) -> rkyv::util::AlignedVec {
    let mut aliases = PathAliases::new();
    aliases.add("@/*", vec!["src/*".to_string()]);
    let mut builder = GraphBuilder::new().with_path_aliases(aliases);
    for (path, source) in tree {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), source.as_bytes())
                .unwrap(),
        );
    }
    rkyv::to_bytes::<rkyv::rancor::Error>(&builder.build()).unwrap()
}

#[test]
fn test_overlay_clean_barrel_path_alias_reexport_matches_reindex() {
    let ts = TypeScriptProvider::new().unwrap();
    let app_calls =
        "import { render } from './index';\nexport function go() { return render(); }\n";
    for barrel in [
        "export * from '@/ui/button';\n",
        "export { render } from '@/ui/button';\n",
    ] {
        let files = [
            ("src/index.ts", barrel),
            (
                "src/ui/button.ts",
                "export function render() { return 1; }\n",
            ),
            ("src/other.ts", "export function render() { return 2; }\n"),
            (
                "src/app.ts",
                "import { render } from './index';\nexport function go() { return 0; }\n",
            ),
        ];
        let moved = "\nexport function render() { return 1; }\n";
        for (label, edits, line) in [
            ("caller", vec![("src/app.ts", app_calls)], 1),
            (
                "caller+target",
                vec![("src/app.ts", app_calls), ("src/ui/button.ts", moved)],
                2,
            ),
        ] {
            let edited: Vec<(&str, &str)> = files
                .iter()
                .map(|&(path, source)| {
                    edits
                        .iter()
                        .find(|(edited, _)| *edited == path)
                        .map_or((path, source), |&(_, new)| (path, new))
                })
                .collect();
            let full = build(&ts, &edited);
            let full = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&full).unwrap();
            let reindex = calls_of_go(&MergedGraph::new(full, None));
            assert_eq!(
                reindex,
                [(
                    "src/ui/button.ts".to_string(),
                    "render".to_string(),
                    line,
                    0.95
                )],
                "{label} {barrel}: the reindex follows the aliased re-export"
            );

            let base = build(&ts, &files);
            let base = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&base).unwrap();
            let inputs: Vec<OverlayFileInput> = edits
                .iter()
                .map(|(path, source)| {
                    ts.parse_file(Path::new(path), source.as_bytes())
                        .unwrap()
                        .into()
                })
                .collect();
            let view = OverlayView::build(base, &inputs).expect("edits make a view");
            assert_eq!(
                calls_of_go(&MergedGraph::new(base, Some(&view))),
                reindex,
                "{label} {barrel}: overlay vs reindex"
            );
        }
    }
}

#[path = "common/parity.rs"]
mod parity;

/// A dirty importer that names its module through a path alias. The overlay
/// expands the specifier through the aliases `graph.bin` keeps, as the index
/// does through its config.
struct AliasFixture {
    label: &'static str,
    /// `(pattern, replacement)`, as the config loader adds them.
    aliases: &'static [(&'static str, &'static str)],
    files: &'static [(&'static str, &'static str)],
    /// The caller's file, its edited source, and the caller's name.
    caller: (&'static str, &'static str, &'static str),
    /// The file the reindex must bind into, or `None` for a parity-only case.
    target: Option<&'static str>,
}

/// `render` has a namesake in another file, so the global tier stays
/// ambiguous: only the alias can bind the call.
const ALIAS_FIXTURES: &[AliasFixture] = &[
    AliasFixture {
        label: "TypeScript/alias",
        aliases: &[("@/*", "src/*")],
        files: &[
            ("src/ui/button.ts", "export function render() { return 1; }\n"),
            ("src/other.ts", "export function render() { return 2; }\n"),
            ("src/app.ts", "import { render } from '@/ui/button';\nexport function go() { return 0; }\n"),
        ],
        caller: ("src/app.ts", "import { render } from '@/ui/button';\nexport function go() { return render(); }\n", "go"),
        target: Some("src/ui/button.ts"),
    },
    AliasFixture {
        label: "JavaScript/alias",
        aliases: &[("@/*", "src/*")],
        files: &[
            ("src/ui/button.js", "export function render() { return 1; }\n"),
            ("src/other.js", "export function render() { return 2; }\n"),
            ("src/app.js", "import { render } from '@/ui/button';\nexport function go() { return 0; }\n"),
        ],
        caller: ("src/app.js", "import { render } from '@/ui/button';\nexport function go() { return render(); }\n", "go"),
        target: Some("src/ui/button.js"),
    },
    // No pattern matches `#/`: both sides treat the specifier as a package.
    AliasFixture {
        label: "TypeScript/alias with no match",
        aliases: &[("@/*", "src/*")],
        files: &[
            ("src/ui/button.ts", "export function render() { return 1; }\n"),
            ("src/other.ts", "export function render() { return 2; }\n"),
            ("src/app.ts", "import { render } from '#/ui/button';\nexport function go() { return 0; }\n"),
        ],
        caller: ("src/app.ts", "import { render } from '#/ui/button';\nexport function go() { return render(); }\n", "go"),
        target: None,
    },
    // The replacement leaves the repo: no indexed file can match it.
    AliasFixture {
        label: "TypeScript/alias outside the repo",
        aliases: &[("@ext/*", "../outside/*")],
        files: &[
            ("src/ui/button.ts", "export function render() { return 1; }\n"),
            ("src/other.ts", "export function render() { return 2; }\n"),
            ("src/app.ts", "import { render } from '@ext/ui/button';\nexport function go() { return 0; }\n"),
        ],
        caller: ("src/app.ts", "import { render } from '@ext/ui/button';\nexport function go() { return render(); }\n", "go"),
        target: None,
    },
    // The go.mod alias (`module/*` → `./*`). Parity only: whether the index
    // places a Go package directory through it is not what this asserts.
    AliasFixture {
        label: "Go/go.mod module alias",
        aliases: &[("example.com/m/*", "./*")],
        files: &[
            ("pkg/util/util.go", "package util\n\nfunc Render() int { return 1 }\n"),
            ("pkg/other/other.go", "package other\n\nfunc Render() int { return 2 }\n"),
            ("cmd/app/main.go", "package main\n\nimport \"example.com/m/pkg/util\"\n\nfunc run() int { return 0 }\n"),
        ],
        caller: ("cmd/app/main.go", "package main\n\nimport \"example.com/m/pkg/util\"\n\nfunc run() int { return util.Render() }\n", "run"),
        target: None,
    },
    // The pyproject alias (`name/*` → `src/name/*`). A dotted Python import
    // matches no slash pattern, so this case cannot show an alias hit; it
    // guards that both sides agree on the miss.
    AliasFixture {
        label: "Python/pyproject alias",
        aliases: &[("shop/*", "src/shop/*")],
        files: &[
            ("src/shop/util.py", "def render():\n    return 1\n"),
            ("src/shop/other.py", "def render():\n    return 2\n"),
            ("app.py", "from shop.util import render\n\ndef run():\n    return 0\n"),
        ],
        caller: ("app.py", "from shop.util import render\n\ndef run():\n    return render()\n", "run"),
        target: None,
    },
];

fn parse_any(path: &str, source: &str) -> ecp_core::analyzer::types::LocalGraph {
    ecp_cli::reanalyze::pipeline()
        .parse_file_raw(Path::new(path), source.as_bytes())
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn test_overlay_dirty_importer_through_path_alias_matches_reindex() {
    let mut failures = Vec::new();
    for fixture in ALIAS_FIXTURES {
        let new_builder = || {
            let mut aliases = PathAliases::new();
            for &(pattern, replacement) in fixture.aliases {
                aliases.add(pattern, vec![replacement.to_string()]);
            }
            GraphBuilder::new().with_path_aliases(aliases)
        };
        let (caller_file, edited, caller) = fixture.caller;
        let target_file = fixture.target.unwrap_or_default();
        let moved_target = fixture
            .target
            .and_then(|target| fixture.files.iter().find(|(path, _)| *path == target))
            .map(|(path, source)| (*path, format!("\n{source}")));
        let mut dirty_sets = vec![("importer", vec![(caller_file, edited.to_string())])];
        if let Some(moved) = moved_target {
            dirty_sets.push((
                "importer+target",
                vec![(caller_file, edited.to_string()), moved],
            ));
        }
        for (dirty_set, edits) in &dirty_sets {
            let edits: Vec<(&str, &str)> = edits.iter().map(|(p, s)| (*p, s.as_str())).collect();
            let parity::EdgeParity { reindex, overlay } = parity::edges_after_edit_with(
                &new_builder,
                &parse_any,
                fixture.files,
                &edits,
                RelType::Calls,
                &|file, name| file == caller_file && name == caller,
            );
            let label = format!("{}/{dirty_set}", fixture.label);
            if fixture.target.is_some()
                && !reindex.iter().any(|e| e.edge.callee_file == target_file)
            {
                failures.push(format!(
                    "{label}: the reindex binds nothing in {target_file}, so the fixture proves nothing: {reindex:#?}"
                ));
                continue;
            }
            // An overlay `Calls` edge carries the uncommitted-edit reason.
            let strip = |edges: &[parity::ReasonedEdge]| -> Vec<parity::CallEdge> {
                let mut calls: Vec<_> = edges.iter().map(|e| e.edge.clone()).collect();
                calls.dedup();
                calls
            };
            if strip(&overlay) != strip(&reindex) {
                failures.push(format!(
                    "{label}: overlay {overlay:#?}\nreindex {reindex:#?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
