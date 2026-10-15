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
