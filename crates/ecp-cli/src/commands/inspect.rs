use crate::commands::format::{node_kind_to_str, rel_type_to_str};
use crate::commands::symbol_id::{resolve_owner_class, split_fqn_target};
use crate::engine::Engine;
use crate::output::{emit_with_caveat, OutputFormat};
use clap::Args;
use ecp_core::file_category::is_test_path;
use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind, RelType};
use ecp_core::session::{MergedGraph, MergedNode};
use ecp_core::EcpError;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

#[derive(Args, Debug)]
pub struct InspectArgs {
    /// Target symbol name (equivalent to `--name` flag).
    pub name: Option<String>,

    /// Named alias for the positional NAME argument — kept for parity with old MCP / wrapper habits.
    #[arg(long = "name", value_name = "NAME", conflicts_with = "name")]
    pub name_flag: Option<String>,

    /// Repository path
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format
    #[arg(long)]
    pub format: Option<String>,

    /// Comma-separated list of node kinds (lowercase, e.g. `function,method`)
    /// to keep on the *target* side of incoming/outgoing edges.
    #[arg(long)]
    pub kind: Option<String>,

    /// Substring filter applied to the target file path of incoming/outgoing
    /// edges. Case-sensitive substring match (not glob).
    #[arg(long = "file", alias = "file_path", alias = "file-path")]
    pub file_path: Option<String>,

    /// Comma-separated list of relation types (lowercase, e.g. `calls,imports`).
    #[arg(long = "relation_types", alias = "relation-types")]
    pub relation_types: Option<String>,

    /// Include edges whose target lives in a test file. Defaults to false.
    #[arg(
        long = "include_tests",
        alias = "include-tests",
        alias = "includeTests",
        default_value_t = false
    )]
    pub include_tests: bool,
}

/// Synthetic-node sentinel for `file_idx`. Annotation (Decorates),
/// EventTopic, TransactionScope etc. don't own a single source file and
/// store `u32::MAX` instead of a real `files[]` index.
const SYNTHETIC_NODE_PATH: &str = "<synthetic>";

/// A merged-space node's file path. Synthetic nodes (file_idx == u32::MAX)
/// resolve to a sentinel string instead of out-of-bounds-panicking.
fn resolve_file_path<'a>(merged: &MergedGraph<'a>, node: &MergedNode<'a>) -> &'a str {
    node.file_path(merged).unwrap_or(SYNTHETIC_NODE_PATH)
}

/// Merged-space node behind an index that came from [`search_nodes`] or a
/// [`MergedGraph`] edge, both of which only yield indices inside merged space.
fn node_at<'a>(merged: &MergedGraph<'a>, idx: u32) -> MergedNode<'a> {
    merged
        .node(idx)
        .expect("search hits and merged edges stay inside merged space")
}

/// Split a `a,b,c` style value into a lower-cased Vec. Trims whitespace and
/// drops empty segments. `None` / empty input → no filter.
fn parse_csv_lower(s: Option<&str>) -> Option<Vec<String>> {
    let raw = s?;
    let parts: Vec<String> = raw
        .split(',')
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts)
    }
}

/// Strip constant-across-all-edges boilerplate from an edge-section map for
/// toon/llm output. Operates on the `incoming`, `outgoing`,
/// `heuristic_incoming`, and `heuristic_outgoing` sections of the inspect
/// result — each is a `{rel_type: [{name, kind, filePath, ...}, ...]}` map.
///
/// Fields removed (when constant):
/// - `tier`: always `"unresolved"` pre-T4-7; dropping saves ~20 bytes/edge
/// - `checks`: always `{}` pre-T4-7; empty object adds token noise without signal
/// - `ownerClass`: dropped when every entry across ALL edge sections has
///   `ownerClass: null`; when any entry has a non-null ownerClass the column
///   is kept because the LLM needs it to distinguish method callers
///
/// Affects only the toon/llm path; json format uses the unstripped block.
fn strip_edge_boilerplate(result: &mut serde_json::Value) {
    // Collect which edge-section keys exist in this result.
    let section_keys = [
        "incoming",
        "outgoing",
        "heuristic_incoming",
        "heuristic_outgoing",
    ];

    // First pass: determine whether any entry has a non-null ownerClass.
    // If so, keep the column so callers can see method ownership.
    let any_non_null_owner = section_keys.iter().any(|&key| {
        result
            .get(key)
            .and_then(|s| s.as_object())
            .map(|map| {
                map.values()
                    .filter_map(|v| v.as_array())
                    .flatten()
                    .any(|entry| {
                        !matches!(
                            entry.get("ownerClass"),
                            Some(serde_json::Value::Null) | None
                        )
                    })
            })
            .unwrap_or(false)
    });

    for &key in &section_keys {
        if let Some(section) = result.get_mut(key).and_then(|s| s.as_object_mut()) {
            for entries in section.values_mut() {
                if let Some(arr) = entries.as_array_mut() {
                    for entry in arr.iter_mut() {
                        if let Some(obj) = entry.as_object_mut() {
                            // Always drop: constant sentinel values.
                            obj.remove("tier");
                            obj.remove("checks");
                            // Conditional: only drop when the column is all-null.
                            if !any_non_null_owner {
                                obj.remove("ownerClass");
                            }
                        }
                    }
                }
            }
        }
    }

    // Also strip ownerClass: null from the symbol header when no edge carries a
    // non-null ownerClass — the null conveys no information and adds noise.
    if !any_non_null_owner {
        if let Some(symbol) = result.get_mut("symbol").and_then(|s| s.as_object_mut()) {
            if matches!(symbol.get("ownerClass"), Some(serde_json::Value::Null)) {
                symbol.remove("ownerClass");
            }
        }
    }
}

/// Per-edge values that never vary today (see `build_inspect_block`). JSON
/// keeps them once at the top level of an ambiguous result instead of on every
/// edge of every match; an edge whose value differs keeps its own copy.
fn edge_defaults() -> serde_json::Value {
    serde_json::json!({ "tier": "unresolved", "checks": {} })
}

fn hoist_edge_defaults(block: &mut serde_json::Value) {
    let defaults = edge_defaults();
    for key in [
        "incoming",
        "outgoing",
        "heuristic_incoming",
        "heuristic_outgoing",
    ] {
        let Some(section) = block.get_mut(key).and_then(|s| s.as_object_mut()) else {
            continue;
        };
        for entry in section
            .values_mut()
            .filter_map(|v| v.as_array_mut())
            .flatten()
        {
            let Some(obj) = entry.as_object_mut() else {
                continue;
            };
            for field in ["tier", "checks"] {
                if obj.get(field) == defaults.get(field) {
                    obj.remove(field);
                }
            }
        }
    }
}

/// Ambiguous result: every match as a full block. `--file` / `--kind` filter a
/// block's neighbours, not the match list, so the hint names what does narrow it.
fn ambiguous_result(
    mut blocks: Vec<serde_json::Value>,
    name: &str,
    omitted_kinds: serde_json::Map<String, serde_json::Value>,
    json: bool,
) -> serde_json::Value {
    let message = format!(
        "Found {} symbols matching '{}'. --file and --kind filter neighbours, not the match. \
         Re-run `ecp inspect --name Owner.{}` using matches[].symbol.ownerClass, \
         or pick a match by matches[].symbol.filePath and startLine.",
        blocks.len(),
        name,
        name
    );
    for block in &mut blocks {
        if json {
            hoist_edge_defaults(block);
        } else {
            strip_edge_boilerplate(block);
        }
    }
    let mut result = serde_json::json!({
        "status": "ambiguous",
        "message": message,
        "matches": blocks,
        "omitted_kinds": omitted_kinds,
    });
    if json {
        result["edge_defaults"] = edge_defaults();
    }
    result
}

/// Build the full inspect payload for a single merged-space node index.
///
/// Returns a JSON object with: symbol (no uid), incoming, outgoing,
/// blind_spots, and impact_upstream_1hop.
fn build_inspect_block(
    merged: &MergedGraph<'_>,
    idx: u32,
    kind_filter: &Option<Vec<String>>,
    rel_filter: &Option<Vec<String>>,
    file_substr: Option<&str>,
    include_tests: bool,
) -> serde_json::Value {
    let node = node_at(merged, idx);
    let file_path_str = resolve_file_path(merged, &node);

    let edge_keeps = |target_kind_str: &str, target_file_path: &str, rel_str: &str| -> bool {
        if let Some(ref kinds) = kind_filter {
            if !kinds
                .iter()
                .any(|k| k == &target_kind_str.to_ascii_lowercase())
            {
                return false;
            }
        }
        if let Some(ref rels) = rel_filter {
            if !rels.iter().any(|r| r == rel_str) {
                return false;
            }
        }
        if let Some(substr) = file_substr {
            if !target_file_path.contains(substr) {
                return false;
            }
        }
        if !include_tests && is_test_path(target_file_path) {
            return false;
        }
        true
    };

    let mut incoming: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let mut outgoing: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let mut heuristic_incoming: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let mut heuristic_outgoing: HashMap<String, Vec<serde_json::Value>> = HashMap::new();

    // Outgoing edges — split deterministic vs heuristic.
    for edge in merged.out_edges(idx) {
        let target_node = node_at(merged, edge.target);
        // Synthetic nodes (Annotation from PR #365 Decorates, EventTopic,
        // TransactionScope) carry `file_idx == u32::MAX` because they don't
        // own a single source file. Resolve to `<synthetic>` instead of
        // indexing past the files array.
        let target_file_path = resolve_file_path(merged, &target_node);
        let target_kind = node_kind_to_str(&target_node.kind());
        let rel = edge.rel_type();
        let rel_str = rel_type_to_str(rel).to_string();

        if !edge_keeps(target_kind, target_file_path, &rel_str) {
            continue;
        }

        let entry = serde_json::json!({
            "name": target_node.name(merged),
            "ownerClass": target_node.owner_class(merged),
            "kind": target_kind,
            "filePath": target_file_path,
            "reason": edge.reason(merged),
            "confidence": edge.confidence(),
            // Mirror `find-schema-bindings`' shape so one agent consuming both
            // commands sees a consistent schema: `tier` is a top-level label,
            // `checks` an object of per-check results. Until T4-7 computes them,
            // tier is the explicit `unresolved` sentinel and checks is empty —
            // both type-stable (T4-7 only fills values, never restructures).
            "tier": "unresolved",
            "checks": {},
        });
        if rel.is_heuristic() {
            heuristic_outgoing.entry(rel_str).or_default().push(entry);
        } else {
            outgoing.entry(rel_str).or_default().push(entry);
        }
    }

    // Incoming edges — split deterministic vs heuristic. A Class / Struct
    // also reads its constructors' in-edges: a construction lands on the sole
    // constructor when there is one, so those callers are the instantiators.
    // Each such entry names the constructor it came through.
    let constructors = merged.constructors_of(idx);
    let read_from = std::iter::once((idx, None)).chain(
        constructors
            .iter()
            .map(|&c| (c, Some(node_at(merged, c).name(merged)))),
    );
    for (read_idx, via_constructor) in read_from {
        for edge in merged.in_edges(read_idx) {
            let rel = edge.rel_type();
            // A constructor's other in-edges (the owning class's `HasMethod`)
            // are not instantiations.
            if via_constructor.is_some() && rel != RelType::Calls {
                continue;
            }
            let source_node = node_at(merged, edge.source);
            let source_file_path = resolve_file_path(merged, &source_node);
            let source_kind = node_kind_to_str(&source_node.kind());
            let rel_str = rel_type_to_str(rel).to_string();

            // For incoming edges the "target" we filter against is the OTHER end —
            // i.e. the caller / importer.
            if !edge_keeps(source_kind, source_file_path, &rel_str) {
                continue;
            }

            let mut entry = serde_json::json!({
                "name": source_node.name(merged),
                "ownerClass": source_node.owner_class(merged),
                "kind": source_kind,
                "filePath": source_file_path,
                "reason": edge.reason(merged),
                "confidence": edge.confidence(),
                // See the outgoing-edge note above: `tier`/`checks` mirror
                // find-schema-bindings; unresolved sentinel + empty object pre-T4-7.
                "tier": "unresolved",
                "checks": {},
            });
            if let Some(ctor) = via_constructor {
                entry["viaConstructor"] = serde_json::Value::from(ctor);
            }
            if rel.is_heuristic() {
                heuristic_incoming.entry(rel_str).or_default().push(entry);
            } else {
                incoming.entry(rel_str).or_default().push(entry);
            }
        }
    }

    // Blind spots: only from the same file. The base graph records them for
    // the committed text of a file, so a dirty file's records point at lines
    // that no longer hold — a virtual node reports none rather than stale ones.
    let blind_spots: Vec<serde_json::Value> = match node {
        MergedNode::Base(_) => merged
            .blind_spots
            .iter()
            .filter(|bs| bs.file_path.resolve(&merged.string_pool) == file_path_str)
            .map(|bs| {
                serde_json::json!({
                    "kind": bs.kind.resolve(&merged.string_pool),
                    "line": bs.start_row.to_native(),
                    "hint": ecp_core::graph::render_hint(
                        bs.hint.resolve(&merged.string_pool),
                        bs.start_row.to_native(),
                    ),
                })
            })
            .collect(),
        MergedNode::Virtual(_) => Vec::new(),
    };

    // 1-hop upstream impact: direct callers. Reuses the same `edge_keeps`
    // policy as `incoming` so the two channels can't drift — empty
    // `incoming` + populated `impact_upstream_1hop` previously contradicted
    // each other for any function whose only callers lived under `tests/`.
    let upstream_1hop = bfs_upstream_1hop(merged, idx, &constructors, &edge_keeps);

    // Class-like derived view: flatten outgoing HasMethod / HasProperty edges
    // into compact member lists. For Enum, additionally surface variants via
    // outgoing Defines→EnumVariant edges (PR #364 EnumVariant + PR #359
    // scope_defines). Variants stay in their own bucket — semantically not
    // properties.
    let kind = node.kind();
    let (contained_methods, contained_properties, contained_variants) = if matches!(
        kind,
        NodeKind::Class | NodeKind::Struct | NodeKind::Trait | NodeKind::Interface | NodeKind::Enum
    ) {
        collect_contained_members(merged, idx)
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };

    // Decorators on this symbol — pulled from FunctionMeta (binary-search
    // on node_idx). `@` prefix stripped to match the cypher `m.decorators`
    // property whitelist behavior (PR #352). Empty for nodes without a
    // FunctionMeta entry, which includes a symbol the overlay added.
    let decorators = node
        .meta_idx(idx)
        .map_or_else(Vec::new, |meta_idx| collect_decorators(merged, meta_idx));

    let has_heuristic = !heuristic_outgoing.is_empty() || !heuristic_incoming.is_empty();

    let mut block = serde_json::json!({
        "symbol": {
            "name": node.name(merged),
            "ownerClass": node.owner_class(merged),
            "kind": node_kind_to_str(&kind),
            "filePath": file_path_str,
            "startLine": node.start_line(),
            "endLine": node.end_line(),
            "decorators": decorators,
        },
        "incoming": incoming,
        "outgoing": outgoing,
        "blind_spots": blind_spots,
        "impact_upstream_1hop": upstream_1hop,
        "contained_methods": contained_methods,
        "contained_properties": contained_properties,
        "contained_variants": contained_variants,
    });

    if has_heuristic {
        let obj = block.as_object_mut().unwrap();
        obj.insert(
            "heuristic_outgoing".to_string(),
            serde_json::json!(heuristic_outgoing),
        );
        obj.insert(
            "heuristic_incoming".to_string(),
            serde_json::json!(heuristic_incoming),
        );
        // Signals LLM consumers that heuristic edges need manual verification.
        obj.insert(
            "heuristic_note".to_string(),
            serde_json::json!("verify before acting — candidate edges, may have false positives"),
        );
    }

    // Field with zero recorded readers: disambiguate "no reader" from "this
    // language doesn't model field reads yet" (JS class fields, Ruby attrs) so
    // an LLM doesn't read empty incoming as "safe to change".
    if kind == NodeKind::Property && !incoming.contains_key("reads_field") {
        block.as_object_mut().unwrap().insert(
            "field_readers_note".to_string(),
            serde_json::json!(
                "no ReadsField edges — either unread, or this language doesn't \
                 capture field reads yet (e.g. JS class fields, Ruby attrs); \
                 grep before assuming no readers"
            ),
        );
    }

    block
}

/// Walk outgoing HasMethod / HasProperty edges and produce flat member lists
/// for `ecp inspect`'s Class view. Skips the test/file filters used by the
/// generic incoming/outgoing buckets — class membership is structural and
/// shouldn't disappear because a method happens to live in a test file.
///
/// The overlay rebuilds no membership edges: a dirty type lists its committed
/// members that still exist (at their working-tree lines), and a type the
/// overlay added lists none.
fn collect_contained_members(
    merged: &MergedGraph<'_>,
    idx: u32,
) -> (
    Vec<serde_json::Value>,
    Vec<serde_json::Value>,
    Vec<serde_json::Value>,
) {
    let mut methods = Vec::new();
    let mut properties = Vec::new();
    let mut variants = Vec::new();
    for edge in merged.out_edges(idx) {
        let target_node = node_at(merged, edge.target);
        // Defines edges can target many kinds; only EnumVariant belongs in the
        // contained-variants bucket. Other Defines targets (File→Function etc.)
        // are not "contained members" of the source — they're scope edges.
        let bucket = match edge.rel_type() {
            RelType::HasMethod => &mut methods,
            RelType::HasProperty => &mut properties,
            RelType::Defines if target_node.kind() == NodeKind::EnumVariant => &mut variants,
            _ => continue,
        };
        bucket.push(serde_json::json!({
            "name": target_node.name(merged),
            "kind": node_kind_to_str(&target_node.kind()),
            "filePath": resolve_file_path(merged, &target_node),
            "line": target_node.start_line(),
        }));
    }
    (methods, properties, variants)
}

/// Resolve a node's decorators via the FunctionMeta binary-search lookup
/// used by the cypher executor (PR #352). Returns the names with `@` prefix
/// stripped — matches the cypher `m.decorators` property convention so
/// LLM consumers see the same shape whether they read inspect JSON or
/// cypher results.
fn collect_decorators(graph: &ArchivedZeroCopyGraph, node_idx: u32) -> Vec<String> {
    match graph
        .function_metas
        .binary_search_by_key(&node_idx, |m| m.node_idx.to_native())
    {
        Ok(i) => graph.function_metas[i]
            .decorators
            .iter()
            .map(|d| {
                let s = d.resolve(&graph.string_pool);
                s.strip_prefix('@').unwrap_or(s).to_string()
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Collect direct symbol-level callers of `idx` (depth=1 upstream).
/// Returns a compact list of `{name, kind, file}` records.
///
/// File-kind sources are skipped: `(File)-[:Imports]->(symbol)` edges are
/// module-level dependencies, not call sites, and surfacing them as
/// "upstream callers" misleads an LLM consumer that asked "who depends on
/// this function". `edge_keeps` is the same closure applied to `incoming`
/// so the two channels expose the same filtered view of the in-edges.
fn bfs_upstream_1hop<F>(
    merged: &MergedGraph<'_>,
    idx: u32,
    constructors: &[u32],
    edge_keeps: &F,
) -> Vec<serde_json::Value>
where
    F: Fn(&str, &str, &str) -> bool,
{
    let mut visited = HashSet::new();
    visited.insert(idx);

    // A Class / Struct is also called through its constructors (see the
    // `incoming` loop in `build_inspect_block`); a caller reaching both counts once.
    let read_from = std::iter::once((idx, false)).chain(constructors.iter().map(|&c| (c, true)));

    let mut queue = VecDeque::new();
    for (read_idx, is_ctor) in read_from {
        for edge in merged.in_edges(read_idx) {
            let rel = edge.rel_type();
            // Constructor in-edges other than `Calls` are not instantiations.
            if is_ctor && rel != RelType::Calls {
                continue;
            }
            let source_node = node_at(merged, edge.source);
            let source_kind = source_node.kind();
            if source_kind == NodeKind::File {
                continue;
            }
            // Synthetic Annotation nodes (Decorates resolver-miss) carry
            // SYNTHETIC_FILE_IDX — skip from incoming traversal display.
            let Some(source_file_path) = source_node.file_path(merged) else {
                continue;
            };
            if !edge_keeps(
                node_kind_to_str(&source_kind),
                source_file_path,
                rel_type_to_str(rel),
            ) {
                continue;
            }
            if visited.insert(edge.source) {
                queue.push_back((source_node, source_file_path));
            }
        }
    }

    queue
        .into_iter()
        .map(|(n, file)| {
            serde_json::json!({
                "name": n.name(merged),
                "kind": node_kind_to_str(&n.kind()),
                "file": file,
            })
        })
        .collect()
}

/// Merged-space indices and kinds of the nodes named `bare_name` (and owned
/// by `owner_filter`): visible base nodes in node order, then the overlay's
/// virtual nodes in view order. A base node the overlay replaced or deleted
/// is not visible — its virtual successor, if any, answers instead, so a
/// dirty-file symbol reports its working-tree span and edges.
fn search_nodes(
    merged: &MergedGraph<'_>,
    bare_name: &str,
    owner_filter: Option<&str>,
) -> Vec<(u32, NodeKind)> {
    let owner_matches = |owner: Option<&str>| owner_filter.is_none_or(|want| owner == Some(want));

    // The name index yields indices ascending, like the scan.
    let mut hits: Vec<(u32, NodeKind)> = merged
        .name_candidates(bare_name)
        .filter(|&i| {
            let node = &merged.nodes[i as usize];
            node.name.resolve(&merged.string_pool) == bare_name
                // Synthetic Annotation nodes (resolver-miss fallback from
                // `decorates_edges`) carry SYNTHETIC_FILE_IDX and have no
                // file:line; they're not meaningful inspect targets.
                && node.has_owning_file()
                && merged.base_visible(i)
                && owner_matches(resolve_owner_class(merged, i as usize))
        })
        .map(|i| (i, NodeKind::from(&merged.nodes[i as usize].kind)))
        .collect();

    if let Some(view) = merged.view() {
        // A file that defines one symbol twice yields two virtual nodes under
        // one uid; the index keeps only the first, so inspect does too.
        let mut seen = rustc_hash::FxHashSet::default();
        hits.extend(
            (view.base_len()..)
                .zip(view.virtual_nodes())
                .filter(|(_, n)| {
                    n.name == bare_name
                        && owner_matches(n.owner_class.as_deref())
                        && seen.insert(n.uid)
                })
                .map(|(i, n)| (i, n.kind)),
        );
    }
    hits
}

pub fn run(args: InspectArgs, engine: &Engine, _graph_path: &Path) -> Result<(), EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());

    let name_query = args
        .name
        .as_deref()
        .or(args.name_flag.as_deref())
        .filter(|s| !s.is_empty());

    if name_query.is_none() {
        return Err(EcpError::InvalidArgument(
            "Target symbol name is required".to_string(),
        ));
    }

    let name = name_query.unwrap();

    // Split `Owner.Method` form for precise targeting.
    let (owner_filter, bare_name) = split_fqn_target(name);

    // Clean queries must not initialize a view or scan the graph for a delta.
    let view = engine.overlay_dir().and_then(|_| engine.overlay_view());
    let merged = MergedGraph::new(graph, view);

    let matching_nodes = search_nodes(&merged, bare_name, owner_filter);

    if matching_nodes.is_empty() {
        let result = serde_json::json!({
            "status": "error",
            "message": format!("Symbol '{}' not found.", name)
        });
        return emit_with_caveat(&result, format, engine.caveat());
    }

    // When the only ambiguity is Impl vs a primary type declaration (Struct /
    // Class / Enum / Trait) sharing the same name, suppress the Impl nodes so
    // `inspect --name Foo` returns the struct, not "ambiguous".  This is the
    // canonical Rust pattern: `struct Foo` + `impl Foo { ... }`.
    let has_primary_type = matching_nodes.iter().any(|(_, kind)| {
        matches!(
            kind,
            NodeKind::Struct
                | NodeKind::Class
                | NodeKind::Enum
                | NodeKind::Trait
                | NodeKind::Interface
        )
    });
    let mut omitted_kinds: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    let matching_nodes: Vec<(u32, NodeKind)> = if has_primary_type {
        let mut impl_count: u64 = 0;
        let kept = matching_nodes
            .into_iter()
            .filter(|(_, kind)| {
                if *kind == NodeKind::Impl {
                    impl_count += 1;
                    false
                } else {
                    true
                }
            })
            .collect();
        if impl_count > 0 {
            omitted_kinds.insert(
                node_kind_to_str(&NodeKind::Impl).to_string(),
                serde_json::json!(impl_count),
            );
        }
        kept
    } else {
        matching_nodes
    };

    // Pre-parse filters once.
    let kind_filter = parse_csv_lower(args.kind.as_deref());
    let rel_filter = parse_csv_lower(args.relation_types.as_deref());
    let file_substr = args.file_path.as_deref().filter(|s| !s.is_empty());

    if matching_nodes.len() == 1 {
        let (node_idx, _) = matching_nodes[0];
        let block = build_inspect_block(
            &merged,
            node_idx,
            &kind_filter,
            &rel_filter,
            file_substr,
            args.include_tests,
        );
        let mut result = serde_json::json!({
            "status": "found",
            "symbol": block["symbol"],
            "incoming": block["incoming"],
            "outgoing": block["outgoing"],
            "processes": [],
            "blind_spots": block["blind_spots"],
            "impact_upstream_1hop": block["impact_upstream_1hop"],
            "contained_methods": block["contained_methods"],
            "contained_properties": block["contained_properties"],
            "contained_variants": block["contained_variants"],
            "omitted_kinds": omitted_kinds,
        });
        if block.get("heuristic_note").is_some() {
            let obj = result.as_object_mut().unwrap();
            obj.insert(
                "heuristic_outgoing".to_string(),
                block["heuristic_outgoing"].clone(),
            );
            obj.insert(
                "heuristic_incoming".to_string(),
                block["heuristic_incoming"].clone(),
            );
            obj.insert(
                "heuristic_note".to_string(),
                block["heuristic_note"].clone(),
            );
        }
        if let Some(note) = block.get("field_readers_note") {
            result
                .as_object_mut()
                .unwrap()
                .insert("field_readers_note".to_string(), note.clone());
        }
        if !omitted_kinds.is_empty() {
            let impl_n = omitted_kinds
                .get("Impl")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            eprintln!(
                "note: {impl_n} Impl node(s) omitted (primary type matched); use `ecp cypher` to query implementors"
            );
        }
        if !matches!(format, OutputFormat::Json) {
            strip_edge_boilerplate(&mut result);
        }
        return emit_with_caveat(&result, format, engine.caveat());
    }

    // Ambiguous: return ALL matches as full inspect blocks (not a candidates list).
    let blocks: Vec<serde_json::Value> = matching_nodes
        .iter()
        .map(|(node_idx, _)| {
            build_inspect_block(
                &merged,
                *node_idx,
                &kind_filter,
                &rel_filter,
                file_substr,
                args.include_tests,
            )
        })
        .collect();

    if !omitted_kinds.is_empty() {
        let impl_n = omitted_kinds
            .get("Impl")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        eprintln!(
            "note: {impl_n} Impl node(s) omitted (primary type matched); use `ecp cypher` to query implementors"
        );
    }
    let result = ambiguous_result(
        blocks,
        name,
        omitted_kinds,
        matches!(format, OutputFormat::Json),
    );
    emit_with_caveat(&result, format, engine.caveat())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::graph::ZeroCopyGraph;
    use ecp_core::graph_fixture::GraphFixture;
    use ecp_core::session::{OverlayFileInput, OverlaySymbol, OverlayView};

    type Bytes = rkyv::util::AlignedVec;

    fn archive(g: &ZeroCopyGraph) -> Bytes {
        rkyv::to_bytes::<rkyv::rancor::Error>(g).unwrap()
    }

    fn access(bytes: &Bytes) -> &ArchivedZeroCopyGraph {
        rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).unwrap()
    }

    /// nodes: 0 Foo.validate (foo.py) · 1 Bar.validate (bar.py) · 2 helper (foo.py)
    fn validate_graph() -> ZeroCopyGraph {
        let mut fx = GraphFixture::new();
        fx.method("src/foo.py", "Foo", "validate");
        fx.method("src/bar.py", "Bar", "validate");
        fx.func("src/foo.py", "helper");
        fx.build()
    }

    fn indices(hits: &[(u32, NodeKind)]) -> Vec<u32> {
        hits.iter().map(|(idx, _)| *idx).collect()
    }

    const DIRTY: &str = "src/dirty.rs";

    /// nodes: 0 helper (clean) · 1 Bar.validate (dirty) · 2 Foo.validate
    /// (clean) · 3 doomed (dirty) · 4 caller (clean)
    /// edges: caller→Bar.validate, caller→doomed, Bar.validate→Foo.validate
    /// (Calls). Bar.validate carries `@cached` and spans line 1; the dirty
    /// file has one blind spot.
    fn dirty_graph() -> ZeroCopyGraph {
        let mut fx = GraphFixture::new();
        fx.func("src/clean.rs", "helper");
        let bar = fx.method(DIRTY, "Bar", "validate");
        let foo = fx.method("src/clean.rs", "Foo", "validate");
        let doomed = fx.func(DIRTY, "doomed");
        let caller = fx.func("src/clean.rs", "caller");
        fx.edge(caller, bar, RelType::Calls);
        fx.edge(caller, doomed, RelType::Calls);
        fx.edge(bar, foo, RelType::Calls);
        fx.function_meta(bar, 0, &["@cached"]);
        fx.blind_spot("dynamic_call", DIRTY, (3, 0, 3, 5), "hint");
        fx.build()
    }

    fn symbol(
        name: &str,
        kind: NodeKind,
        owner: Option<&str>,
        lines: (u32, u32),
        calls: &[&str],
    ) -> OverlaySymbol {
        OverlaySymbol {
            is_exported: false,
            name: name.to_string(),
            kind,
            owner_class: owner.map(str::to_string),
            start_line: lines.0,
            end_line: lines.1,
            start_column: 0,
            end_column: 0,
            calls: calls.iter().map(|c| c.to_string()).collect(),
            heritage: Vec::new(),
            variant_refs: Vec::new(),
            return_type: None,
        }
    }

    /// The dirty file re-parsed: `Bar.validate` moved to lines 10-14 and now
    /// calls `helper`; `fresh` (16-18) is new and calls `helper`; `doomed`
    /// is gone.
    fn dirty_input() -> OverlayFileInput {
        OverlayFileInput {
            export_aliases: Vec::new(),
            rel_path: DIRTY.to_string(),
            symbols: vec![
                symbol(
                    "validate",
                    NodeKind::Method,
                    Some("Bar"),
                    (10, 14),
                    &["helper"],
                ),
                symbol("fresh", NodeKind::Function, None, (16, 18), &["helper"]),
            ],
            imports: vec![],
        }
    }

    fn virtual_idx(merged: &MergedGraph<'_>, name: &str) -> u32 {
        let view = merged.view().expect("a dirty view");
        (view.base_len()..)
            .zip(view.virtual_nodes())
            .find(|(_, n)| n.name == name)
            .map(|(i, _)| i)
            .unwrap_or_else(|| panic!("no virtual node named {name}"))
    }

    fn with_dirty_view<F: FnOnce(MergedGraph<'_>)>(f: F) {
        let bytes = archive(&dirty_graph());
        let graph = access(&bytes);
        let view = OverlayView::build(graph, &[dirty_input()]).expect("dirty view");
        f(MergedGraph::new(graph, Some(&view)));
    }

    fn block(merged: &MergedGraph<'_>, idx: u32) -> serde_json::Value {
        build_inspect_block(merged, idx, &None, &None, None, false)
    }

    /// Sorted names under `block[section][rel]`; empty when the bucket is absent.
    fn names(block: &serde_json::Value, section: &str, rel: &str) -> Vec<String> {
        let mut out: Vec<String> = block[section][rel]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .map(|e| e["name"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }

    fn upstream_names(block: &serde_json::Value) -> Vec<String> {
        let mut out: Vec<String> = block["impact_upstream_1hop"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn test_search_nodes_without_view_returns_base_matches_in_node_order() {
        let bytes = archive(&validate_graph());
        let merged = MergedGraph::new(access(&bytes), None);
        assert_eq!(indices(&search_nodes(&merged, "validate", None)), [0, 1]);
        assert_eq!(
            indices(&search_nodes(&merged, "validate", Some("Bar"))),
            [1]
        );
        assert!(search_nodes(&merged, "missing", None).is_empty());
    }

    /// A namesake in a clean file and one in a dirty file are both listed:
    /// the clean one at its base index, the dirty one at its virtual index.
    #[test]
    fn test_search_nodes_clean_and_dirty_namesakes_lists_both() {
        with_dirty_view(|merged| {
            let dirty = virtual_idx(&merged, "validate");
            assert_eq!(
                search_nodes(&merged, "validate", None),
                [(2, NodeKind::Method), (dirty, NodeKind::Method)]
            );
        });
    }

    #[test]
    fn test_search_nodes_owner_filter_on_virtual_method_matches_virtual() {
        with_dirty_view(|merged| {
            let dirty = virtual_idx(&merged, "validate");
            assert_eq!(
                indices(&search_nodes(&merged, "validate", Some("Bar"))),
                [dirty]
            );
            assert_eq!(
                indices(&search_nodes(&merged, "validate", Some("Foo"))),
                [2]
            );
            assert!(search_nodes(&merged, "validate", Some("Nobody")).is_empty());
        });
    }

    #[test]
    fn test_search_nodes_deleted_dirty_symbol_not_found() {
        with_dirty_view(|merged| {
            assert!(search_nodes(&merged, "doomed", None).is_empty());
        });
    }

    #[test]
    fn test_search_nodes_overlay_only_symbol_found() {
        with_dirty_view(|merged| {
            let fresh = virtual_idx(&merged, "fresh");
            assert_eq!(
                search_nodes(&merged, "fresh", None),
                [(fresh, NodeKind::Function)]
            );
        });
    }

    /// A dirty file that defines the function `doomed` twice (both branches
    /// of an `if`) yields two virtual nodes under the base symbol's uid. The
    /// index keeps the first. Methods differ: a later one is an overload with
    /// its own uid, and both stay listed.
    #[test]
    fn test_search_nodes_same_uid_redefinition_lists_first_only() {
        let bytes = archive(&dirty_graph());
        let graph = access(&bytes);
        let mut input = dirty_input();
        for lines in [(20, 22), (24, 26)] {
            input
                .symbols
                .push(symbol("doomed", NodeKind::Function, None, lines, &[]));
        }
        let view = OverlayView::build(graph, &[input]).expect("dirty view");
        let merged = MergedGraph::new(graph, Some(&view));
        let first = virtual_idx(&merged, "doomed");
        assert_eq!(indices(&search_nodes(&merged, "doomed", None)), [first]);
    }

    /// Rejected or corrupt fragments leave no accepted input, and an empty
    /// input set builds no view: the answer is the base graph's, deleted
    /// symbol included.
    #[test]
    fn test_search_nodes_rejected_fragment_answers_from_base() {
        let bytes = archive(&dirty_graph());
        let graph = access(&bytes);
        let view = OverlayView::build(graph, &[]);
        assert!(view.is_none());
        let merged = MergedGraph::new(graph, view.as_ref());
        assert_eq!(indices(&search_nodes(&merged, "validate", None)), [1, 2]);
        assert_eq!(indices(&search_nodes(&merged, "doomed", None)), [3]);
        assert!(search_nodes(&merged, "fresh", None).is_empty());
    }

    #[test]
    fn test_build_inspect_block_without_view_reports_base_span_and_edges() {
        let bytes = archive(&dirty_graph());
        let merged = MergedGraph::new(access(&bytes), None);
        let b = block(&merged, 1);
        assert_eq!(b["symbol"]["filePath"], DIRTY);
        assert_eq!(b["symbol"]["startLine"], 1);
        assert_eq!(b["symbol"]["ownerClass"], "Bar");
        assert_eq!(b["symbol"]["decorators"], serde_json::json!(["cached"]));
        assert_eq!(names(&b, "outgoing", "calls"), ["validate"]);
        assert_eq!(b["outgoing"]["calls"][0]["ownerClass"], "Foo");
        assert_eq!(names(&b, "incoming", "calls"), ["caller"]);
        assert_eq!(b["incoming"]["calls"][0]["confidence"], 1.0);
        assert_eq!(b["incoming"]["calls"][0]["reason"], "ast-call");
        assert_eq!(upstream_names(&b), ["caller"]);
        assert_eq!(b["blind_spots"].as_array().unwrap().len(), 1);
    }

    /// A surviving dirty-file symbol answers with its working-tree span, the
    /// overlay's out-edges (the stale `Foo.validate` call is gone) and its
    /// clean callers. Its base blind spots describe the committed text, so
    /// none are reported; its decorators come from the uid-identical twin.
    #[test]
    fn test_build_inspect_block_virtual_twin_reports_worktree_span_and_overlay_edges() {
        with_dirty_view(|merged| {
            let b = block(&merged, virtual_idx(&merged, "validate"));
            assert_eq!(b["symbol"]["name"], "validate");
            assert_eq!(b["symbol"]["ownerClass"], "Bar");
            assert_eq!(b["symbol"]["kind"], "Method");
            assert_eq!(b["symbol"]["filePath"], DIRTY);
            assert_eq!(b["symbol"]["startLine"], 10);
            assert_eq!(b["symbol"]["endLine"], 14);
            assert_eq!(b["symbol"]["decorators"], serde_json::json!(["cached"]));
            assert_eq!(names(&b, "outgoing", "calls"), ["helper"]);
            assert_eq!(names(&b, "incoming", "calls"), ["caller"]);
            assert_eq!(upstream_names(&b), ["caller"]);
            assert!(b["blind_spots"].as_array().unwrap().is_empty());
        });
    }

    /// The `--file` filter applies to a virtual node's edges like any other.
    #[test]
    fn test_build_inspect_block_virtual_twin_file_filter_drops_unmatched_edges() {
        with_dirty_view(|merged| {
            let idx = virtual_idx(&merged, "validate");
            let kept = build_inspect_block(&merged, idx, &None, &None, Some("clean.rs"), false);
            assert_eq!(names(&kept, "outgoing", "calls"), ["helper"]);
            let dropped = build_inspect_block(&merged, idx, &None, &None, Some("nowhere"), false);
            assert!(names(&dropped, "outgoing", "calls").is_empty());
            assert!(names(&dropped, "incoming", "calls").is_empty());
            assert!(upstream_names(&dropped).is_empty());
        });
    }

    #[test]
    fn test_build_inspect_block_overlay_only_symbol_gets_full_block() {
        with_dirty_view(|merged| {
            let b = block(&merged, virtual_idx(&merged, "fresh"));
            assert_eq!(b["symbol"]["kind"], "Function");
            assert_eq!(b["symbol"]["filePath"], DIRTY);
            assert_eq!(b["symbol"]["startLine"], 16);
            assert_eq!(b["symbol"]["endLine"], 18);
            assert_eq!(b["symbol"]["decorators"], serde_json::json!([]));
            assert_eq!(names(&b, "outgoing", "calls"), ["helper"]);
            assert!(names(&b, "incoming", "calls").is_empty());
            assert!(upstream_names(&b).is_empty());
        });
    }

    /// A clean symbol's callers include the dirty file's current callers.
    #[test]
    fn test_build_inspect_block_clean_callee_lists_overlay_callers() {
        with_dirty_view(|merged| {
            let b = block(&merged, 0);
            assert_eq!(names(&b, "incoming", "calls"), ["fresh", "validate"]);
            assert_eq!(upstream_names(&b), ["fresh", "validate"]);
        });
    }

    /// "dup" repeats across kinds, owners and files; "shared" has 1200 nodes
    /// (see `same_name_nodes`). Also a tombstone (empty name) and a unicode
    /// name.
    fn same_name_graph() -> ZeroCopyGraph {
        let mut fx = GraphFixture::new();
        fx.same_name_nodes(
            "shared",
            1200,
            |i| format!("src/s{i}.ts"),
            |fx, i, path, _| {
                if i % 100 == 0 {
                    fx.func(path, "");
                    fx.func(path, "naïve_函数");
                    fx.func(path, "dup");
                    fx.method(path, "Owner", "dup");
                    fx.method(path, "Other", "dup");
                }
            },
        );
        fx.build()
    }

    /// `src/s0.ts` re-parsed: `shared` and `dup` survive, an `Owner.shared`
    /// method is new, every other symbol of the file is gone.
    fn same_name_dirty_input() -> OverlayFileInput {
        OverlayFileInput {
            export_aliases: Vec::new(),
            rel_path: "src/s0.ts".to_string(),
            symbols: vec![
                symbol("shared", NodeKind::Function, None, (1, 2), &[]),
                symbol("dup", NodeKind::Function, None, (3, 4), &[]),
                symbol("shared", NodeKind::Method, Some("Owner"), (5, 6), &[]),
            ],
            imports: vec![],
        }
    }

    /// Same hits, indices and order from the name index as from the full scan
    /// that clearing `name_index` forces, with and without a view.
    #[test]
    fn test_search_nodes_with_index_matches_full_scan() {
        let fast = same_name_graph();
        let mut slow = same_name_graph();
        slow.name_index.clear();
        assert!(!fast.name_index.is_empty());
        let fast_bytes = archive(&fast);
        let slow_bytes = archive(&slow);
        let fast = access(&fast_bytes);
        let slow = access(&slow_bytes);
        let fast_view = OverlayView::build(fast, &[same_name_dirty_input()]).unwrap();
        let slow_view = OverlayView::build(slow, &[same_name_dirty_input()]).unwrap();

        let cases: [(&str, Option<&str>); 8] = [
            ("shared", None),
            ("shared", Some("Owner")),
            ("shared", Some("Nobody")),
            ("dup", None),
            ("dup", Some("Other")),
            ("naïve_函数", None),
            ("absent", None),
            ("", None),
        ];
        for (fast_view, slow_view) in [(None, None), (Some(&fast_view), Some(&slow_view))] {
            let fast = MergedGraph::new(fast, fast_view);
            let slow = MergedGraph::new(slow, slow_view);
            for (name, owner) in cases {
                assert_eq!(
                    search_nodes(&fast, name, owner),
                    search_nodes(&slow, name, owner),
                    "{name:?} owner={owner:?} view={}",
                    fast_view.is_some()
                );
            }
        }
    }

    #[test]
    fn test_search_nodes_shared_name_lists_every_node_ascending() {
        let bytes = archive(&same_name_graph());
        let merged = MergedGraph::new(access(&bytes), None);
        let hits = indices(&search_nodes(&merged, "shared", None));
        assert_eq!(hits.len(), 1200);
        assert!(hits.windows(2).all(|w| w[0] < w[1]), "node order");
    }

    /// The tombstone has an empty name but still owns a file: only the scan
    /// matches it, so an empty bare name must keep the scan.
    #[test]
    fn test_search_nodes_empty_bare_name_keeps_tombstone_matches() {
        let bytes = archive(&same_name_graph());
        let merged = MergedGraph::new(access(&bytes), None);
        assert_eq!(search_nodes(&merged, "", None).len(), 12);
    }

    fn block_with_edge(owner: &str) -> serde_json::Value {
        serde_json::json!({
            "symbol": {"name": "validate", "ownerClass": owner, "kind": "Method",
                       "filePath": "src/a.py", "startLine": 3},
            "incoming": {"calls": [
                {"name": "c", "tier": "unresolved", "checks": {}},
                {"name": "d", "tier": "verified", "checks": {"x": true}},
            ]},
            "outgoing": {},
        })
    }

    #[test]
    fn test_ambiguous_result_hint_does_not_point_at_neighbour_filters() {
        let r = ambiguous_result(
            vec![block_with_edge("Foo")],
            "validate",
            Default::default(),
            true,
        );
        let msg = r["message"].as_str().unwrap();
        assert!(!msg.contains("to disambiguate"), "{msg}");
        assert!(
            msg.contains("Owner.validate") && msg.contains("ownerClass"),
            "{msg}"
        );
    }

    #[test]
    fn test_ambiguous_result_json_hoists_constant_edge_keys_once() {
        let r = ambiguous_result(
            vec![block_with_edge("Foo")],
            "validate",
            Default::default(),
            true,
        );
        assert_eq!(r["edge_defaults"], edge_defaults());
        let calls = &r["matches"][0]["incoming"]["calls"];
        assert!(calls[0].get("tier").is_none() && calls[0].get("checks").is_none());
        // A non-default edge keeps its own values, and the symbol keeps ownerClass.
        assert_eq!(calls[1]["tier"], "verified");
        assert_eq!(calls[1]["checks"], serde_json::json!({"x": true}));
        assert_eq!(r["matches"][0]["symbol"]["ownerClass"], "Foo");
    }

    #[test]
    fn test_ambiguous_result_toon_has_no_edge_defaults_key() {
        let r = ambiguous_result(
            vec![block_with_edge("Foo")],
            "validate",
            Default::default(),
            false,
        );
        assert!(r.get("edge_defaults").is_none());
    }
}
