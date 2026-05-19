//! Integration test: TypeScript tsconfig.json `compilerOptions.paths`
//! expansion. When a project declares `"@/*": ["src/*"]`, the resolver
//! must expand `@/utils` to `src/utils` (then walk extensions / index
//! suffixes) and route the import edge to the correct file.

use cgn_analyzer::resolution::builder::GraphBuilder;
use cgn_analyzer::resolution::path_aliases::PathAliases;
use cgn_analyzer::typescript::TypeScriptProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::graph::{NodeKind, RelType};

#[test]
fn alias_specifier_resolves_to_aliased_file_e2e() {
    // tsconfig.json equivalent:
    //   { "compilerOptions": { "baseUrl": ".", "paths": { "@/*": ["src/*"] } } }
    let mut aliases = PathAliases::new();
    aliases.add("@/*", vec!["src/*".to_string()]);

    let provider = TypeScriptProvider::new().unwrap();
    let utils = provider
        .parse_file(
            "src/utils.ts".as_ref(),
            include_str!("fixtures/tsconfig_paths_utils.ts").as_bytes(),
        )
        .unwrap();
    let main = provider
        .parse_file(
            "src/main.ts".as_ref(),
            include_str!("fixtures/tsconfig_paths_main.ts").as_bytes(),
        )
        .unwrap();

    let mut builder = GraphBuilder::new().with_path_aliases(aliases);
    builder.add_graph(utils);
    builder.add_graph(main);
    let graph = builder.build();

    let pool = &graph.string_pool;
    let name_of = |s: cgn_core::pool::StrRef| -> &str {
        let start = s.offset as usize;
        std::str::from_utf8(&pool[start..start + s.len as usize]).expect("utf-8 pool")
    };
    let file_of = |idx: u32| {
        let file_idx = graph.nodes[idx as usize].file_idx as usize;
        let path_ref = graph.files[file_idx].path;
        let start = path_ref.offset as usize;
        std::str::from_utf8(&pool[start..start + path_ref.len as usize]).expect("utf-8 pool")
    };

    let caller_id = graph
        .nodes
        .iter()
        .position(|n| {
            name_of(n.name) == "main" && matches!(n.kind, NodeKind::Function | NodeKind::Method)
        })
        .expect("main() function missing");

    let util_id = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(idx, n)| {
            name_of(n.name) == "utilFn"
                && matches!(n.kind, NodeKind::Function | NodeKind::Method)
                && file_of(*idx as u32) == "src/utils.ts"
        })
        .map(|(idx, _)| idx)
        .expect("utilFn (src/utils.ts) missing");

    let calls_edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls && e.source == caller_id as u32)
        .collect();

    assert!(
        calls_edges.iter().any(|e| e.target == util_id as u32),
        "expected CALLS edge main → utilFn through @/utils alias; got targets {:?}",
        calls_edges
            .iter()
            .map(|e| (e.target, name_of(graph.nodes[e.target as usize].name)))
            .collect::<Vec<_>>(),
    );
}

#[test]
fn no_alias_with_ambiguous_global_yields_no_resolution_baseline() {
    // Pin the negative baseline: without alias expansion AND with a
    // second same-named export elsewhere (so Tier-3 global unique-lookup
    // refuses), the import `@/utils` cannot resolve to any CALLS edge.
    // This isolates the alias feature's contribution from the existing
    // Tier-3 unique-name safety net.
    let provider = TypeScriptProvider::new().unwrap();
    let utils = provider
        .parse_file(
            "src/utils.ts".as_ref(),
            include_str!("fixtures/tsconfig_paths_utils.ts").as_bytes(),
        )
        .unwrap();
    // Duplicate `utilFn` in another non-vendor file → Tier-3 unique-global refuses.
    let dup_utils = provider
        .parse_file(
            "lib/utils.ts".as_ref(),
            include_str!("fixtures/tsconfig_paths_utils.ts").as_bytes(),
        )
        .unwrap();
    let main = provider
        .parse_file(
            "src/main.ts".as_ref(),
            include_str!("fixtures/tsconfig_paths_main.ts").as_bytes(),
        )
        .unwrap();

    let mut builder = GraphBuilder::new();
    builder.add_graph(utils);
    builder.add_graph(dup_utils);
    builder.add_graph(main);
    let graph = builder.build();

    let pool = &graph.string_pool;
    let name_of = |s: cgn_core::pool::StrRef| -> &str {
        let start = s.offset as usize;
        std::str::from_utf8(&pool[start..start + s.len as usize]).expect("utf-8 pool")
    };
    let caller_id = graph
        .nodes
        .iter()
        .position(|n| name_of(n.name) == "main")
        .expect("main missing");
    let has_calls_edge = graph
        .edges
        .iter()
        .any(|e| e.source == caller_id as u32 && e.rel_type == RelType::Calls);
    assert!(
        !has_calls_edge,
        "without aliases + ambiguous global, @/utils must NOT resolve (refuse-to-guess)",
    );
}
