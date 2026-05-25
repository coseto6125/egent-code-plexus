//! Per-symbol content-hash diffing for incremental reindex (T7-6).
//!
//! After `reanalyze_files` reparsed changed files, this module compares
//! each symbol's fresh `content_hash` against the copy stored in the
//! previous `ZeroCopyGraph`. Symbols whose body is identical skip the
//! resolver + class_membership pass entirely.
//!
//! ## Guards — skip must NOT happen when any of these hold
//!
//! - **(a) Import-set changed** — a new import can resolve a previously-
//!   unresolved call. All symbols in the file fall through to full reanalyze.
//! - **(b) Shadow-candidate set changed** — a sibling file was added/removed
//!   that shadows an existing JS/TS import. All symbols fall through.
//! - **(c) SchemaField name-set changed** — a `SchemaField` was added, removed,
//!   or renamed in the file. Because `schema_field_mirrors` buckets by
//!   `(name.to_lowercase(), SchemaType)`, a stable name-set means
//!   `MirrorsField` emission for this file is unaffected by partial-resolve.
//!   Type-only changes are rare in practice; the file's resolver pass
//!   re-runs anyway when symbol bodies move, so a type rename inside an
//!   unchanged body name is caught via guard (a) (import set) or per-symbol
//!   hash diff.
//!   Pre-v10: any file with non-empty `schema_fields` forced FullReanalyze
//!   (conservative fallback). Post-v10: compare new vs old name set per file.

use ecp_core::analyzer::types::{LocalGraph, RawImport};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::PathBuf;

/// Decision for a single file in the incremental diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDiffDecision {
    /// All guards clear; only the listed symbol UIDs need resolver work.
    /// UIDs absent from this set can skip Pass 2 + class_membership entirely.
    /// An empty set means every symbol in the file is unchanged — the file
    /// contributes zero resolver work.
    PartialResolve { changed_uids: FxHashSet<u64> },
    /// At least one guard fired; run the full resolver for this file.
    FullReanalyze { reason: SkipGuard },
}

/// Which guard prevented a skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipGuard {
    /// Guard (a): the file's import set changed.
    ImportSetChanged,
    /// Guard (b): shadow-candidate set changed (a sibling JS/TS file was
    /// added/removed that can steal import resolution from this file).
    ShadowCandidatesChanged,
    /// Guard (c) conservative fallback: file has `schema_fields` set, so
    /// `MirrorsField` bucket membership COULD have changed. Deferred to T7-7.
    SchemaFieldPresent,
}

/// Output of `symbol_hash_diff`: per-file decisions + aggregate counters
/// used by callers to report skip efficiency.
pub struct SymbolHashDiff {
    /// Map from forward-slash relative file path → diff decision.
    pub decisions: FxHashMap<String, FileDiffDecision>,
    /// Number of symbols that will be skipped (not re-resolved).
    pub skipped_count: usize,
    /// Number of symbols that need resolver work.
    pub resolve_count: usize,
}

/// Compute the per-symbol content-hash diff between the old graph and the
/// freshly-parsed `new_graphs`.
///
/// `old_symbol_hashes` is a mapping `node_uid → content_hash` built from
/// the old `ZeroCopyGraph` (see `build_old_hash_map`). It covers ALL files
/// in the old graph; only entries whose file path appears in `new_graphs`
/// are used.
///
/// `old_import_sets` is a mapping from forward-slash path → sorted import
/// source+name pairs from the old graph's `LocalGraph`s (or from the
/// archived `imports_edges` data). Callers that don't have this available
/// can pass an empty map — missing entries cause guard (a) to fire
/// conservatively (full reanalyze for that file).
///
/// `originally_changed` is the set of files that were EXPLICITLY changed
/// (before shadow expansion). Files in `new_graphs` whose path is NOT in
/// `originally_changed` were pulled in as shadow candidates by
/// `reanalyze_files` — they get guard (b) automatically because the
/// shadowing relationship itself changed. Pass an empty slice to treat all
/// `new_graphs` files as explicitly changed (disables guard (b)).
///
/// `old_schema_field_names` is a per-file map of `SchemaField` names from
/// the old graph (use `schema_field_names_per_file` to build from an
/// `ArchivedZeroCopyGraph`). When the new file's schema_field name-set
/// matches the old, guard (c) clears. Pass an empty map to retain the
/// pre-v10 conservative fallback (any non-empty schema_fields →
/// FullReanalyze).
pub fn symbol_hash_diff(
    old_symbol_hashes: &FxHashMap<u64, u64>,
    old_import_sets: &FxHashMap<String, Vec<(String, String)>>,
    old_schema_field_names: &FxHashMap<String, FxHashSet<String>>,
    new_graphs: &[LocalGraph],
    originally_changed: &[PathBuf],
) -> SymbolHashDiff {
    let mut decisions: FxHashMap<String, FileDiffDecision> =
        FxHashMap::with_capacity_and_hasher(new_graphs.len(), Default::default());
    let mut skipped_count = 0usize;
    let mut resolve_count = 0usize;

    // Guard (b): build a set of originally-changed paths for O(1) lookup.
    // Files in `new_graphs` that are NOT in this set were shadow-included by
    // `reanalyze_files` — the shadowing relationship changed, so they must
    // run full reanalyze regardless of their body hashes.
    let changed_set: FxHashSet<PathBuf> = if originally_changed.is_empty() {
        // Treat all new_graphs as explicitly changed — guard (b) disabled.
        new_graphs.iter().map(|g| g.file_path.clone()).collect()
    } else {
        originally_changed.iter().cloned().collect()
    };

    for lg in new_graphs {
        let raw_path = lg.file_path.to_string_lossy();
        let path_str: String = if raw_path.contains('\\') {
            raw_path.replace('\\', "/")
        } else {
            raw_path.into_owned()
        };

        // Guard (c): compare new vs old SchemaField name-set per file.
        // Conservative fallback kicks in only when `old_schema_field_names`
        // has no entry for the file — caller didn't supply it (legacy path).
        if let Some(sf) = lg.schema_fields.as_ref() {
            if !sf.is_empty() {
                let new_names: FxHashSet<String> = sf.iter().map(|f| f.name.to_string()).collect();
                let names_changed = match old_schema_field_names.get(&path_str) {
                    Some(old) => &new_names != old,
                    // No entry from caller → fall back to v9 conservative behaviour.
                    None => true,
                };
                if names_changed {
                    resolve_count += lg.nodes.len();
                    decisions.insert(
                        path_str,
                        FileDiffDecision::FullReanalyze {
                            reason: SkipGuard::SchemaFieldPresent,
                        },
                    );
                    continue;
                }
                // Names stable → guard (c) clears; fall through to guard (a)/(b).
            }
        }

        // Guard (b): file was shadow-included (not originally changed) →
        // its import-resolution candidates have changed.
        if !changed_set.contains(&lg.file_path) {
            resolve_count += lg.nodes.len();
            decisions.insert(
                path_str,
                FileDiffDecision::FullReanalyze {
                    reason: SkipGuard::ShadowCandidatesChanged,
                },
            );
            continue;
        }

        // Guard (a): compare import sets.
        let new_import_set = import_set_key(&lg.imports);
        let guard_a_fires = match old_import_sets.get(&path_str) {
            // Old entry present: fire only when the sets actually differ.
            Some(old_set) => new_import_set != *old_set,
            // No old entry → first time we see this file, treat as "changed".
            None => !new_import_set.is_empty(),
        };
        if guard_a_fires {
            resolve_count += lg.nodes.len();
            decisions.insert(
                path_str,
                FileDiffDecision::FullReanalyze {
                    reason: SkipGuard::ImportSetChanged,
                },
            );
            continue;
        }

        // All guards clear — diff per-symbol content_hash.
        let mut changed_uids: FxHashSet<u64> = FxHashSet::default();
        for node in &lg.nodes {
            // Synthetic nodes (content_hash == 0) are always re-emitted —
            // their "body" is derived from surrounding context, not raw bytes.
            if node.content_hash == 0 {
                changed_uids.insert(node_uid_for(lg, node));
                resolve_count += 1;
                continue;
            }
            let uid = node_uid_for(lg, node);
            match old_symbol_hashes.get(&uid) {
                Some(&old_hash) if old_hash == node.content_hash => {
                    // Unchanged body — skip.
                    skipped_count += 1;
                }
                _ => {
                    // New symbol or hash mismatch — resolve.
                    changed_uids.insert(uid);
                    resolve_count += 1;
                }
            }
        }
        decisions.insert(path_str, FileDiffDecision::PartialResolve { changed_uids });
    }

    SymbolHashDiff {
        decisions,
        skipped_count,
        resolve_count,
    }
}

/// Build a `(uid → content_hash)` map from an owned `ZeroCopyGraph`'s nodes
/// for use as the `old_symbol_hashes` argument to `symbol_hash_diff`.
///
/// The map covers all symbol nodes (excludes `NodeKind::File` / `Process` /
/// `Route` / `EntryPoint` synthetic nodes whose `content_hash` is always 0).
pub fn build_old_hash_map(nodes: &[ecp_core::graph::Node]) -> FxHashMap<u64, u64> {
    nodes
        .iter()
        .filter(|n| n.content_hash != 0)
        .map(|n| (n.uid, n.content_hash))
        .collect()
}

/// Build the old SchemaField name-set per file from an archived graph for
/// use as the `old_schema_field_names` argument to `symbol_hash_diff`.
/// Uses the v10 `nodes_by_kind(SchemaField)` CSR — O(SchemaField count),
/// not O(N). Falls back to a linear scan on legacy v9 graphs.
pub fn schema_field_names_per_file(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
) -> FxHashMap<String, FxHashSet<String>> {
    let mut out: FxHashMap<String, FxHashSet<String>> = FxHashMap::default();
    for idx in graph.nodes_by_kind(ecp_core::graph::NodeKind::SchemaField) {
        let n = &graph.nodes[idx as usize];
        if !n.has_owning_file() {
            continue;
        }
        let raw_path = graph.files[n.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool);
        let path_str = if raw_path.contains('\\') {
            raw_path.replace('\\', "/")
        } else {
            raw_path.to_owned()
        };
        out.entry(path_str)
            .or_default()
            .insert(n.name.resolve(&graph.string_pool).to_string());
    }
    out
}

/// Build the old import-set map from a slice of `LocalGraph`s (the parse
/// results stored in the previous analysis pass, if available). Each entry
/// is `path → sorted (source, name)` pairs.
pub fn build_old_import_map(old_graphs: &[LocalGraph]) -> FxHashMap<String, Vec<(String, String)>> {
    old_graphs
        .iter()
        .map(|lg| {
            let path = lg.file_path.to_string_lossy().replace('\\', "/");
            (path, import_set_key(&lg.imports))
        })
        .collect()
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Deterministic (source, imported_name) pair list, sorted for stable
/// equality comparison. Ignores `alias` because an alias rename does NOT
/// change which symbols can be resolved — only the local binding name changes,
/// not the cross-file call resolution.
fn import_set_key(imports: &[RawImport]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = imports
        .iter()
        .map(|i| (i.source.clone(), i.imported_name.clone()))
        .collect();
    pairs.sort_unstable();
    pairs
}

/// Compute the canonical xxh3-64 UID for a `RawNode` using the same
/// algorithm as `GraphBuilder` Pass 1 (`ecp_core::uid::compute`).
fn node_uid_for(lg: &LocalGraph, node: &ecp_core::analyzer::types::RawNode) -> u64 {
    let raw_path = lg.file_path.to_string_lossy();
    let path_str: std::borrow::Cow<'_, str> = if raw_path.contains('\\') {
        std::borrow::Cow::Owned(raw_path.replace('\\', "/"))
    } else {
        raw_path
    };
    ecp_core::uid::compute(
        node.kind,
        &path_str,
        node.owner_class.as_deref(),
        &node.name,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
    use ecp_core::graph::NodeKind;

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
            field_reads: Vec::new(),
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
            path_literals: None,
            call_metas: vec![],
            raw_function_metas: vec![],
        }
    }

    #[test]
    fn test_unchanged_body_skips_resolver() {
        let node = rn("my_func", NodeKind::Function, 0xABCD_1234);
        let graph = lg("src/a.py", vec![node.clone()], vec![]);
        let uid = node_uid_for(&graph, &graph.nodes[0]);

        let mut old_hashes = FxHashMap::default();
        old_hashes.insert(uid, 0xABCD_1234u64);

        let result = symbol_hash_diff(
            &old_hashes,
            &FxHashMap::default(),
            &FxHashMap::default(),
            &[graph],
            &[],
        );
        assert_eq!(
            result.skipped_count, 1,
            "unchanged symbol should be skipped"
        );
        assert_eq!(result.resolve_count, 0);
        let dec = result.decisions.get("src/a.py").unwrap();
        assert!(
            matches!(dec, FileDiffDecision::PartialResolve { changed_uids } if changed_uids.is_empty())
        );
    }

    #[test]
    fn test_guard_c_clears_when_schema_field_names_unchanged() {
        use ecp_core::analyzer::types::{FrameworkId, RawSchemaField, SchemaType};

        let node = rn("User", NodeKind::Class, 0xDEAD_BEEF);
        let mut graph = lg("src/models.py", vec![node.clone()], vec![]);
        let uid = node_uid_for(&graph, &graph.nodes[0]);

        // New file has two schema fields: name + email.
        graph.schema_fields = Some(
            vec![
                RawSchemaField {
                    name: "name".into(),
                    type_class: SchemaType::String,
                    owner_class: "User".into(),
                    framework: FrameworkId::Pydantic,
                    span: (1, 0, 1, 4),
                },
                RawSchemaField {
                    name: "email".into(),
                    type_class: SchemaType::String,
                    owner_class: "User".into(),
                    framework: FrameworkId::Pydantic,
                    span: (2, 0, 2, 5),
                },
            ]
            .into_boxed_slice(),
        );

        let mut old_hashes = FxHashMap::default();
        old_hashes.insert(uid, 0xDEAD_BEEFu64);

        // Old graph's schema field name-set matches new exactly.
        let mut old_schema_names = FxHashMap::default();
        let mut names_for_file = FxHashSet::default();
        names_for_file.insert("name".to_string());
        names_for_file.insert("email".to_string());
        old_schema_names.insert("src/models.py".to_string(), names_for_file);

        let result = symbol_hash_diff(
            &old_hashes,
            &FxHashMap::default(),
            &old_schema_names,
            &[graph],
            &[],
        );

        let dec = result.decisions.get("src/models.py").unwrap();
        assert!(
            matches!(dec, FileDiffDecision::PartialResolve { .. }),
            "guard (c) should clear when schema field names are unchanged; got {dec:?}"
        );
    }

    #[test]
    fn test_guard_c_fires_when_schema_field_added() {
        use ecp_core::analyzer::types::{FrameworkId, RawSchemaField, SchemaType};

        let node = rn("User", NodeKind::Class, 0xCAFE_BABE);
        let mut graph = lg("src/models.py", vec![node.clone()], vec![]);

        // New file has a field that wasn't in the old.
        graph.schema_fields = Some(
            vec![RawSchemaField {
                name: "email_verified".into(),
                type_class: SchemaType::Bool,
                owner_class: "User".into(),
                framework: FrameworkId::Pydantic,
                span: (3, 0, 3, 14),
            }]
            .into_boxed_slice(),
        );

        let mut old_schema_names = FxHashMap::default();
        let mut names_for_file = FxHashSet::default();
        names_for_file.insert("email".to_string());
        old_schema_names.insert("src/models.py".to_string(), names_for_file);

        let result = symbol_hash_diff(
            &FxHashMap::default(),
            &FxHashMap::default(),
            &old_schema_names,
            &[graph],
            &[],
        );

        let dec = result.decisions.get("src/models.py").unwrap();
        assert!(
            matches!(
                dec,
                FileDiffDecision::FullReanalyze {
                    reason: SkipGuard::SchemaFieldPresent
                }
            ),
            "guard (c) should fire when schema field name-set changed; got {dec:?}"
        );
    }

    #[test]
    fn test_one_of_five_edit_only_resolves_one() {
        let nodes: Vec<RawNode> = (1u64..=5)
            .map(|i| rn(&format!("fn{i}"), NodeKind::Function, i * 1000))
            .collect();
        let graph = lg("src/b.py", nodes.clone(), vec![]);

        // Build old hashes: fn3 gets a different hash → changed.
        let mut old_hashes: FxHashMap<u64, u64> = FxHashMap::default();
        for (i, node) in nodes.iter().enumerate() {
            let uid = node_uid_for(&graph, node);
            let hash = if i == 2 {
                node.content_hash + 1 // fn3 body changed
            } else {
                node.content_hash
            };
            old_hashes.insert(uid, hash);
        }

        let result = symbol_hash_diff(
            &old_hashes,
            &FxHashMap::default(),
            &FxHashMap::default(),
            &[graph],
            &[],
        );
        assert_eq!(result.skipped_count, 4, "fn1/fn2/fn4/fn5 should be skipped");
        assert_eq!(result.resolve_count, 1, "only fn3 needs resolve");
        let dec = result.decisions.get("src/b.py").unwrap();
        if let FileDiffDecision::PartialResolve { changed_uids } = dec {
            assert_eq!(changed_uids.len(), 1);
        } else {
            panic!("expected PartialResolve");
        }
    }

    #[test]
    fn test_skip_guarded_when_import_set_changes() {
        let nodes: Vec<RawNode> = (1u64..=5)
            .map(|i| rn(&format!("fn{i}"), NodeKind::Function, i * 1000))
            .collect();
        // File has a new import `foo` — no body hashes changed.
        let new_imports = vec![imp("./foo", "foo")];
        let graph = lg("src/c.py", nodes.clone(), new_imports);

        // Old graph had no imports for this file.
        let old_import_map: FxHashMap<String, Vec<(String, String)>> =
            FxHashMap::from_iter([("src/c.py".to_string(), vec![])]);

        let mut old_hashes: FxHashMap<u64, u64> = FxHashMap::default();
        for node in &nodes {
            let uid = node_uid_for(&graph, node);
            old_hashes.insert(uid, node.content_hash);
        }

        let result = symbol_hash_diff(
            &old_hashes,
            &old_import_map,
            &FxHashMap::default(),
            &[graph],
            &[],
        );
        // Guard (a) fires → full reanalyze despite unchanged body hashes.
        assert_eq!(result.skipped_count, 0);
        assert_eq!(result.resolve_count, 5);
        let dec = result.decisions.get("src/c.py").unwrap();
        assert!(matches!(
            dec,
            FileDiffDecision::FullReanalyze {
                reason: SkipGuard::ImportSetChanged
            }
        ));
    }

    #[test]
    fn test_skip_guarded_when_shadow_candidates_change() {
        // `src/utils.ts` was newly added. `reanalyze_files` shadow-expanded
        // to also include `src/utils.js`. In `symbol_hash_diff`:
        // - `originally_changed` = [utils.ts]
        // - `new_graphs` = [utils.js (shadow-included), utils.ts]
        // → utils.js is NOT in `originally_changed` → guard (b) fires.
        let js_nodes = vec![rn("helper", NodeKind::Function, 0x1111)];
        let js_graph = lg("src/utils.js", js_nodes.clone(), vec![]);
        let ts_graph = lg(
            "src/utils.ts",
            vec![rn("helper", NodeKind::Function, 0x2222)],
            vec![],
        );

        // Old hash for js file matches.
        let mut old_hashes: FxHashMap<u64, u64> = FxHashMap::default();
        old_hashes.insert(node_uid_for(&js_graph, &js_nodes[0]), 0x1111);

        // Only utils.ts was originally changed; utils.js was shadow-included.
        let originally_changed = vec![PathBuf::from("src/utils.ts")];

        let result = symbol_hash_diff(
            &old_hashes,
            &FxHashMap::default(),
            &FxHashMap::default(),
            &[js_graph, ts_graph],
            &originally_changed,
        );

        let dec = result.decisions.get("src/utils.js").unwrap();
        assert!(
            matches!(
                dec,
                FileDiffDecision::FullReanalyze {
                    reason: SkipGuard::ShadowCandidatesChanged
                }
            ),
            "utils.js must be forced to full reanalyze — not in originally_changed: {dec:?}"
        );
    }

    #[test]
    fn test_skip_guarded_when_schema_field_present() {
        use ecp_core::analyzer::types::{FrameworkId, RawSchemaField, SchemaType};
        let node = rn("User", NodeKind::Class, 0xBEEF);
        let mut graph = lg("models/user.py", vec![node.clone()], vec![]);
        // Add a SchemaField — guard (c) conservative fallback must fire.
        graph.schema_fields = Some(Box::new([RawSchemaField {
            owner_class: "User".to_string().into_boxed_str(),
            name: "email".to_string().into_boxed_str(),
            type_class: SchemaType::String,
            framework: FrameworkId::Pydantic,
            span: (2, 0, 2, 20),
        }]));

        let uid = node_uid_for(&graph, &graph.nodes[0]);
        let mut old_hashes = FxHashMap::default();
        old_hashes.insert(uid, 0xBEEFu64); // hash matches — would normally skip

        let result = symbol_hash_diff(
            &old_hashes,
            &FxHashMap::default(),
            &FxHashMap::default(),
            &[graph],
            &[],
        );
        // Guard (c) conservative fallback fires.
        assert_eq!(result.skipped_count, 0);
        let dec = result.decisions.get("models/user.py").unwrap();
        assert!(matches!(
            dec,
            FileDiffDecision::FullReanalyze {
                reason: SkipGuard::SchemaFieldPresent
            }
        ));
    }

    #[test]
    fn test_skip_does_not_drop_existing_edges() {
        // Verify: when all symbols skip, the `changed_uids` set is empty —
        // NO edges are silently dropped; the caller is responsible for carrying
        // forward old edges for unchanged symbols. This test pins the contract
        // that `PartialResolve { changed_uids: empty }` means "preserve old
        // graph edges" not "emit no edges".
        let node = rn("stable_fn", NodeKind::Function, 0xC0FFEE);
        let graph = lg("src/stable.ts", vec![node.clone()], vec![]);
        let uid = node_uid_for(&graph, &graph.nodes[0]);

        let mut old_hashes = FxHashMap::default();
        old_hashes.insert(uid, 0xC0FFEEu64);

        let old_imports = FxHashMap::from_iter([(
            "src/stable.ts".to_string(),
            vec![("./dep".to_string(), "Dep".to_string())],
        )]);
        let new_imports = vec![imp("./dep", "Dep")]; // same as old
        let mut g = lg("src/stable.ts", vec![node.clone()], new_imports);
        // Reuse the same path
        g.file_path = PathBuf::from("src/stable.ts");

        let result = symbol_hash_diff(&old_hashes, &old_imports, &FxHashMap::default(), &[g], &[]);
        assert_eq!(result.skipped_count, 1);
        assert_eq!(result.resolve_count, 0);
        let dec = result.decisions.get("src/stable.ts").unwrap();
        // Empty changed_uids signals "preserve all old edges for this file"
        assert!(
            matches!(dec, FileDiffDecision::PartialResolve { changed_uids } if changed_uids.is_empty()),
            "stable file should produce empty changed_uids, got {dec:?}"
        );
    }
}
