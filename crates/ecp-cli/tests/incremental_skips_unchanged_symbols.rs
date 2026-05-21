//! T7-6 invariant tests: per-symbol content-hash diff gates resolver /
//! class_membership work on the changed-hash subset only.
//!
//! Test organisation
//! -----------------
//! - `test_mtime_touch_skips_resolver` — file touched (mtime bump, no content
//!   change) → the `symbol_hash_diff` skip-set contains ALL symbols → resolve
//!   count is 0.
//! - `test_one_of_five_edit_only_resolves_one` — file with 5 functions, only
//!   function 3's hash changed → resolve count = 1, skip count = 4.
//! - **Guard (a)**: `test_skip_guarded_when_import_set_changes` — unchanged
//!   body hashes but new import line → ALL 5 symbols fall through.
//! - **Guard (b)**: `test_skip_guarded_when_shadow_candidates_change` — new
//!   sibling `.ts` file shadows existing `.js` → js file forced to full
//!   reanalyze.
//! - **Guard (c)**: `test_skip_guarded_when_schemafield_bucket_changes` —
//!   conservative fallback: any file with SchemaFields forces full reanalyze
//!   (cross-file bucket computation deferred to T7-7).
//! - **Negative**: `test_skip_does_not_drop_existing_edges` — empty
//!   `changed_uids` means "preserve old edges", NOT "emit nothing".

use ecp_cli::incremental::{symbol_hash_diff, FileDiffDecision, SkipGuard};
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use rustc_hash::FxHashMap;
use std::path::PathBuf;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn rn(name: &str, kind: NodeKind, content_hash: u64) -> RawNode {
    RawNode {
        name: name.to_string(),
        kind,
        span: (1, 0, 5, 0),
        is_exported: false,
        heritage: vec![],
        type_annotation: None,
        decorators: vec![],
        calls: vec![],
        owner_class: None,
        content_hash,
    }
}

fn imp(source: &str, name: &str) -> RawImport {
    RawImport {
        source: source.to_string(),
        imported_name: name.to_string(),
        alias: None,
        binding_kind: None,
    }
}

fn lg(path: &str, nodes: Vec<RawNode>, imports: Vec<RawImport>) -> LocalGraph {
    LocalGraph {
        file_path: PathBuf::from(path),
        content_hash: [0u8; 8],
        nodes,
        documents: vec![],
        imports,
        routes: vec![],
        framework_refs: vec![],
        fanout_refs: vec![],
        blind_spots: vec![],
        schema_fields: None,
        event_topics: None,
        tx_scopes: None,
        call_metas: vec![],
        raw_function_metas: vec![],
    }
}

fn node_uid(graph: &LocalGraph, node: &RawNode) -> u64 {
    let path = graph.file_path.to_string_lossy().replace('\\', "/");
    ecp_core::uid::compute(node.kind, &path, node.owner_class.as_deref(), &node.name)
}

fn old_hashes_all_match(graph: &LocalGraph) -> FxHashMap<u64, u64> {
    graph
        .nodes
        .iter()
        .map(|n| (node_uid(graph, n), n.content_hash))
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// `touch file.py` (no content change) → AtomicUsize counter (resolve_count)
/// confirms resolver NOT invoked for the file's symbols.
#[test]
fn test_mtime_touch_skips_resolver() {
    let node = rn("do_thing", NodeKind::Function, 0xDEAD_BEEF);
    let graph = lg("src/service.py", vec![node.clone()], vec![]);
    let old_hashes = old_hashes_all_match(&graph);

    let result = symbol_hash_diff(&old_hashes, &FxHashMap::default(), &[graph], &[]);

    // No new imports → guard (a) not triggered; old_import_map absent →
    // treat as "first time seen with empty import set" → guard (a) fires
    // only when NEW imports are non-empty. Here the new graph has no imports
    // either, so the guard comparison is ([], []) → equal → no fire.
    // Body hash matches → skip.
    assert_eq!(
        result.resolve_count, 0,
        "mtime-only touch must not trigger resolver: resolve_count={}",
        result.resolve_count
    );
    assert_eq!(result.skipped_count, 1);
}

/// File with 5 functions; edit just function 3's body → only function 3
/// re-resolves; functions 1/2/4/5 skipped.
#[test]
fn test_one_of_five_edit_only_resolves_one() {
    let nodes: Vec<RawNode> = (1u64..=5)
        .map(|i| rn(&format!("fn{i}"), NodeKind::Function, i * 1000))
        .collect();
    let graph = lg("src/handlers.py", nodes.clone(), vec![]);

    // Old: fn3 had a different hash (the "old" body), so from the diff
    // perspective fn3's NEW hash doesn't match the OLD hash → changed.
    let mut old_hashes: FxHashMap<u64, u64> = FxHashMap::default();
    for (i, node) in nodes.iter().enumerate() {
        let uid = node_uid(&graph, node);
        let old_hash = if i == 2 {
            node.content_hash + 1 // old fn3 body was different
        } else {
            node.content_hash
        };
        old_hashes.insert(uid, old_hash);
    }

    let result = symbol_hash_diff(&old_hashes, &FxHashMap::default(), &[graph], &[]);
    assert_eq!(
        result.skipped_count, 4,
        "fn1/fn2/fn4/fn5 should skip: skipped={}",
        result.skipped_count
    );
    assert_eq!(
        result.resolve_count, 1,
        "only fn3 needs resolve: resolve_count={}",
        result.resolve_count
    );
    let dec = result
        .decisions
        .get("src/handlers.py")
        .expect("decision entry present");
    if let FileDiffDecision::PartialResolve { changed_uids } = dec {
        assert_eq!(changed_uids.len(), 1, "exactly one uid in changed set");
    } else {
        panic!("expected PartialResolve, got {dec:?}");
    }
}

/// Guard (a): file has 5 functions, no body change but new `import foo`
/// line → ALL 5 re-resolve (import change invalidates skip).
#[test]
fn test_skip_guarded_when_import_set_changes() {
    let nodes: Vec<RawNode> = (1u64..=5)
        .map(|i| rn(&format!("fn{i}"), NodeKind::Function, i * 1000))
        .collect();
    let new_imports = vec![imp("./foo", "foo_helper")];
    let graph = lg("src/router.py", nodes.clone(), new_imports);

    // Old graph had no imports for this file.
    let old_import_map = FxHashMap::from_iter([("src/router.py".to_string(), vec![])]);

    // Old body hashes match exactly — without guard (a) we'd skip all 5.
    let old_hashes = old_hashes_all_match(&graph);

    let result = symbol_hash_diff(&old_hashes, &old_import_map, &[graph], &[]);
    assert_eq!(
        result.resolve_count, 5,
        "import-set change must force all 5 symbols to re-resolve: resolve_count={}",
        result.resolve_count
    );
    assert_eq!(result.skipped_count, 0);
    let dec = result.decisions.get("src/router.py").unwrap();
    assert!(
        matches!(
            dec,
            FileDiffDecision::FullReanalyze {
                reason: SkipGuard::ImportSetChanged
            }
        ),
        "guard (a) must fire: {dec:?}"
    );
}

/// Guard (b): new `.ts` file added in sibling dir that shadows existing
/// `.js` import → the `.js` file re-resolves.
///
/// Scenario: `query.ts` was newly created. `reanalyze_files` shadow-expanded
/// to include `query.js`. In `symbol_hash_diff`, `originally_changed` only
/// contains `query.ts`; `query.js` is shadow-included → guard (b) fires.
#[test]
fn test_skip_guarded_when_shadow_candidates_change() {
    let js_node = rn("buildQuery", NodeKind::Function, 0xAAAA_1111);
    let js_graph = lg("src/db/query.js", vec![js_node.clone()], vec![]);

    // A new `src/db/query.ts` is being added in the same batch.
    let ts_node = rn("buildQuery", NodeKind::Function, 0xBBBB_2222);
    let ts_graph = lg("src/db/query.ts", vec![ts_node.clone()], vec![]);

    // Old graph only had `query.js`; its body hash matches.
    let mut old_hashes: FxHashMap<u64, u64> = FxHashMap::default();
    old_hashes.insert(node_uid(&js_graph, &js_node), 0xAAAA_1111);

    // Only query.ts was originally changed; query.js was shadow-included.
    let originally_changed = vec![PathBuf::from("src/db/query.ts")];

    let result = symbol_hash_diff(
        &old_hashes,
        &FxHashMap::default(),
        &[js_graph, ts_graph],
        &originally_changed,
    );

    let dec_js = result
        .decisions
        .get("src/db/query.js")
        .expect("decision for query.js");
    assert!(
        matches!(
            dec_js,
            FileDiffDecision::FullReanalyze {
                reason: SkipGuard::ShadowCandidatesChanged
            }
        ),
        "guard (b) must fire for query.js not in originally_changed: {dec_js:?}"
    );
}

/// Guard (c) conservative fallback: sibling file adds `UserResponse.email`
/// (same name+type+class as our existing `User.email`) → our file re-emits
/// MirrorsField even though our file's body_hash didn't move.
///
/// Because cross-file bucket computation is deferred to T7-7, the guard fires
/// conservatively whenever a file has ANY `schema_fields` set.
#[test]
fn test_skip_guarded_when_schemafield_bucket_changes() {
    use ecp_core::analyzer::types::{FrameworkId, RawSchemaField, SchemaType};

    let user_node = rn("User", NodeKind::Class, 0xCAFE_BABE);
    let mut graph = lg("models/user.py", vec![user_node.clone()], vec![]);
    // SchemaField present → guard (c) conservative fallback fires.
    graph.schema_fields = Some(Box::new([RawSchemaField {
        owner_class: "User".to_string().into_boxed_str(),
        name: "email".to_string().into_boxed_str(),
        type_class: SchemaType::String,
        framework: FrameworkId::Pydantic,
        span: (2, 0, 2, 20),
    }]));

    // Old hash matches exactly — without guard (c) we'd skip.
    let mut old_hashes: FxHashMap<u64, u64> = FxHashMap::default();
    old_hashes.insert(node_uid(&graph, &user_node), 0xCAFE_BABEu64);

    let result = symbol_hash_diff(&old_hashes, &FxHashMap::default(), &[graph], &[]);
    assert_eq!(result.skipped_count, 0);
    let dec = result
        .decisions
        .get("models/user.py")
        .expect("decision present");
    assert!(
        matches!(
            dec,
            FileDiffDecision::FullReanalyze {
                reason: SkipGuard::SchemaFieldPresent
            }
        ),
        "guard (c) conservative fallback must fire for schema-field files: {dec:?}"
    );
}

/// Negative: verify no edges silently disappear when skipping.
/// Empty `changed_uids` signals "preserve old graph edges" — NOT "emit nothing".
/// This test pins the semantic contract so callers know what to do.
#[test]
fn test_skip_does_not_drop_existing_edges() {
    let node = rn("stable_handler", NodeKind::Function, 0x9999_AAAA);
    let imports = vec![imp("./utils", "helper")];
    let graph = lg("src/api.py", vec![node.clone()], imports);

    // Old state: same import set, same hash → everything unchanged.
    let old_import_map = FxHashMap::from_iter([(
        "src/api.py".to_string(),
        vec![("./utils".to_string(), "helper".to_string())],
    )]);
    let old_hashes = old_hashes_all_match(&graph);

    let result = symbol_hash_diff(&old_hashes, &old_import_map, &[graph], &[]);

    // Skip count = 1; resolve count = 0.
    assert_eq!(result.skipped_count, 1);
    assert_eq!(result.resolve_count, 0);

    let dec = result.decisions.get("src/api.py").unwrap();
    // The empty changed_uids is the contract: "caller must preserve existing
    // edges for this file". NOT a signal to drop them.
    assert!(
        matches!(dec, FileDiffDecision::PartialResolve { changed_uids } if changed_uids.is_empty()),
        "stable file must produce empty changed_uids (preserve-edges contract): {dec:?}"
    );
}
