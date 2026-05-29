use crate::commands::format::{kind_to_str, rel_to_str};
use crate::commands::symbol_id::{resolve_owner_class, split_fqn_target};
use crate::engine::Engine;
use crate::output::{emit_with_caveat, OutputFormat};
use crate::session::overlay_reader::load_overlay;
use clap::Args;
use ecp_core::algorithms::process_trace::is_test_path;
use ecp_core::graph::ArchivedZeroCopyGraph;
use ecp_core::session::merge_archived;
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
    #[arg(long = "file_path", alias = "file-path")]
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

/// Safe lookup of a node's file path. Synthetic nodes (file_idx == u32::MAX)
/// resolve to a sentinel string instead of out-of-bounds-panicking.
fn resolve_file_path(graph: &ArchivedZeroCopyGraph, file_idx: u32) -> &str {
    if file_idx == u32::MAX {
        return SYNTHETIC_NODE_PATH;
    }
    graph
        .files
        .get(file_idx as usize)
        .map(|f| f.path.resolve(&graph.string_pool))
        .unwrap_or(SYNTHETIC_NODE_PATH)
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

/// Build the full inspect payload for a single node index.
///
/// Returns a JSON object with: symbol (no uid), incoming, outgoing,
/// blind_spots, and impact_upstream_1hop.
fn build_inspect_block(
    graph: &ArchivedZeroCopyGraph,
    node_idx: usize,
    kind_filter: &Option<Vec<String>>,
    rel_filter: &Option<Vec<String>>,
    file_substr: Option<&str>,
    include_tests: bool,
) -> serde_json::Value {
    let node = &graph.nodes[node_idx];
    let file_node = &graph.files[node.file_idx.to_native() as usize];
    let file_path_str = file_node.path.resolve(&graph.string_pool);

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
    let out_start = graph.out_offsets[node_idx].to_native() as usize;
    let out_end = graph.out_offsets[node_idx + 1].to_native() as usize;
    for i in out_start..out_end {
        let edge = &graph.edges[i];
        let target_node = &graph.nodes[edge.target.to_native() as usize];
        // Synthetic nodes (Annotation from PR #365 Decorates, EventTopic,
        // TransactionScope) carry `file_idx == u32::MAX` because they don't
        // own a single source file. Resolve to `<synthetic>` instead of
        // indexing past the files array.
        let target_file_path = resolve_file_path(graph, target_node.file_idx.to_native());
        let target_kind = kind_to_str(&target_node.kind);
        let rel_str = rel_to_str(&edge.rel_type).to_string();

        if !edge_keeps(target_kind, target_file_path, &rel_str) {
            continue;
        }

        let target_idx = edge.target.to_native() as usize;
        let target_owner = resolve_owner_class(graph, target_idx);
        let entry = serde_json::json!({
            "name": target_node.name.resolve(&graph.string_pool),
            "ownerClass": target_owner,
            "kind": target_kind,
            "filePath": target_file_path,
            "reason": edge.reason.resolve(&graph.string_pool),
            "confidence": edge.confidence.to_native(),
            // Mirror `find-schema-bindings`' shape so one agent consuming both
            // commands sees a consistent schema: `tier` is a top-level label,
            // `checks` an object of per-check results. Until T4-7 computes them,
            // tier is the explicit `unresolved` sentinel and checks is empty —
            // both type-stable (T4-7 only fills values, never restructures).
            "tier": "unresolved",
            "checks": {},
        });
        if edge.rel_type.is_heuristic() {
            heuristic_outgoing.entry(rel_str).or_default().push(entry);
        } else {
            outgoing.entry(rel_str).or_default().push(entry);
        }
    }

    // Incoming edges — split deterministic vs heuristic.
    let in_start = graph.in_offsets[node_idx].to_native() as usize;
    let in_end = graph.in_offsets[node_idx + 1].to_native() as usize;
    for i in in_start..in_end {
        let edge_idx = graph.in_edge_idx[i].to_native() as usize;
        let edge = &graph.edges[edge_idx];
        let source_node = &graph.nodes[edge.source.to_native() as usize];
        let source_file_path = resolve_file_path(graph, source_node.file_idx.to_native());
        let source_kind = kind_to_str(&source_node.kind);
        let rel_str = rel_to_str(&edge.rel_type).to_string();

        // For incoming edges the "target" we filter against is the OTHER end —
        // i.e. the caller / importer.
        if !edge_keeps(source_kind, source_file_path, &rel_str) {
            continue;
        }

        let source_idx = edge.source.to_native() as usize;
        let source_owner = resolve_owner_class(graph, source_idx);
        let entry = serde_json::json!({
            "name": source_node.name.resolve(&graph.string_pool),
            "ownerClass": source_owner,
            "kind": source_kind,
            "filePath": source_file_path,
            "reason": edge.reason.resolve(&graph.string_pool),
            "confidence": edge.confidence.to_native(),
            // See the outgoing-edge note above: `tier`/`checks` mirror
            // find-schema-bindings; unresolved sentinel + empty object pre-T4-7.
            "tier": "unresolved",
            "checks": {},
        });
        if edge.rel_type.is_heuristic() {
            heuristic_incoming.entry(rel_str).or_default().push(entry);
        } else {
            incoming.entry(rel_str).or_default().push(entry);
        }
    }

    // Blind spots: only from the same file.
    let blind_spots: Vec<serde_json::Value> = graph
        .blind_spots
        .iter()
        .filter(|bs| bs.file_path.resolve(&graph.string_pool) == file_path_str)
        .map(|bs| {
            serde_json::json!({
                "kind": bs.kind.resolve(&graph.string_pool),
                "line": bs.start_row.to_native(),
                "hint": bs.hint.resolve(&graph.string_pool),
            })
        })
        .collect();

    // 1-hop upstream impact: direct callers. Reuses the same `edge_keeps`
    // policy as `incoming` so the two channels can't drift — empty
    // `incoming` + populated `impact_upstream_1hop` previously contradicted
    // each other for any function whose only callers lived under `tests/`.
    let upstream_1hop = bfs_upstream_1hop(graph, node_idx, &edge_keeps);

    // Class-like derived view: flatten outgoing HasMethod / HasProperty edges
    // into compact member lists. For Enum, additionally surface variants via
    // outgoing Defines→EnumVariant edges (PR #364 EnumVariant + PR #359
    // scope_defines). Variants stay in their own bucket — semantically not
    // properties.
    let (contained_methods, contained_properties, contained_variants) = if matches!(
        node.kind,
        ecp_core::graph::ArchivedNodeKind::Class
            | ecp_core::graph::ArchivedNodeKind::Struct
            | ecp_core::graph::ArchivedNodeKind::Trait
            | ecp_core::graph::ArchivedNodeKind::Interface
            | ecp_core::graph::ArchivedNodeKind::Enum
    ) {
        collect_contained_members(graph, node_idx)
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };

    // Decorators on this symbol — pulled from FunctionMeta (binary-search
    // on node_idx). `@` prefix stripped to match the cypher `m.decorators`
    // property whitelist behavior (PR #352). Empty for nodes without a
    // FunctionMeta entry.
    let decorators = collect_decorators(graph, node_idx as u32);

    let has_heuristic = !heuristic_outgoing.is_empty() || !heuristic_incoming.is_empty();

    let owner_class = resolve_owner_class(graph, node_idx);
    let mut block = serde_json::json!({
        "symbol": {
            "name": node.name.resolve(&graph.string_pool),
            "ownerClass": owner_class,
            "kind": kind_to_str(&node.kind),
            "filePath": file_path_str,
            "startLine": node.start_line(),
            "endLine": node.span.2.to_native(),
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
    if matches!(node.kind, ecp_core::graph::ArchivedNodeKind::Property)
        && !incoming.contains_key("reads_field")
    {
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
fn collect_contained_members(
    graph: &ArchivedZeroCopyGraph,
    node_idx: usize,
) -> (
    Vec<serde_json::Value>,
    Vec<serde_json::Value>,
    Vec<serde_json::Value>,
) {
    let mut methods = Vec::new();
    let mut properties = Vec::new();
    let mut variants = Vec::new();
    let out_start = graph.out_offsets[node_idx].to_native() as usize;
    let out_end = graph.out_offsets[node_idx + 1].to_native() as usize;
    for i in out_start..out_end {
        let edge = &graph.edges[i];
        let target_node = &graph.nodes[edge.target.to_native() as usize];
        // Defines edges can target many kinds; only EnumVariant belongs in the
        // contained-variants bucket. Other Defines targets (File→Function etc.)
        // are not "contained members" of the source — they're scope edges.
        let bucket = match edge.rel_type {
            ecp_core::graph::ArchivedRelType::HasMethod => &mut methods,
            ecp_core::graph::ArchivedRelType::HasProperty => &mut properties,
            ecp_core::graph::ArchivedRelType::Defines
                if matches!(
                    target_node.kind,
                    ecp_core::graph::ArchivedNodeKind::EnumVariant
                ) =>
            {
                &mut variants
            }
            _ => continue,
        };
        let target_file_path = resolve_file_path(graph, target_node.file_idx.to_native());
        bucket.push(serde_json::json!({
            "name": target_node.name.resolve(&graph.string_pool),
            "kind": kind_to_str(&target_node.kind),
            "filePath": target_file_path,
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

/// Collect direct symbol-level callers of `node_idx` (depth=1 upstream).
/// Returns a compact list of `{name, kind, file}` records.
///
/// File-kind sources are skipped: `(File)-[:Imports]->(symbol)` edges are
/// module-level dependencies, not call sites, and surfacing them as
/// "upstream callers" misleads an LLM consumer that asked "who depends on
/// this function". `edge_keeps` is the same closure applied to `incoming`
/// so the two channels expose the same filtered view of the in-edges.
fn bfs_upstream_1hop<F>(
    graph: &ArchivedZeroCopyGraph,
    node_idx: usize,
    edge_keeps: &F,
) -> Vec<serde_json::Value>
where
    F: Fn(&str, &str, &str) -> bool,
{
    let mut visited = HashSet::new();
    visited.insert(node_idx);

    let in_start = graph.in_offsets[node_idx].to_native() as usize;
    let in_end = graph.in_offsets[node_idx + 1].to_native() as usize;

    let mut queue = VecDeque::new();
    for i in in_start..in_end {
        let edge_idx = graph.in_edge_idx[i].to_native() as usize;
        let edge = &graph.edges[edge_idx];
        let src_idx = edge.source.to_native() as usize;
        let source_node = &graph.nodes[src_idx];
        if matches!(source_node.kind, ecp_core::graph::ArchivedNodeKind::File) {
            continue;
        }
        // Synthetic Annotation nodes (Decorates resolver-miss) carry
        // SYNTHETIC_FILE_IDX — skip from incoming traversal display.
        if !source_node.has_owning_file() {
            continue;
        }
        let source_file = &graph.files[source_node.file_idx.to_native() as usize];
        let source_file_path = source_file.path.resolve(&graph.string_pool);
        let source_kind = kind_to_str(&source_node.kind);
        let rel_str = rel_to_str(&edge.rel_type);
        if !edge_keeps(source_kind, source_file_path, rel_str) {
            continue;
        }
        if visited.insert(src_idx) {
            queue.push_back(src_idx);
        }
    }

    let mut results = Vec::new();
    while let Some(idx) = queue.pop_front() {
        let n = &graph.nodes[idx];
        if !n.has_owning_file() {
            continue;
        }
        let file = &graph.files[n.file_idx.to_native() as usize];
        results.push(serde_json::json!({
            "name": n.name.resolve(&graph.string_pool),
            "kind": kind_to_str(&n.kind),
            "file": file.path.resolve(&graph.string_pool),
        }));
    }

    results
}

/// Find all base-graph indices whose node name and optional owner match.
///
/// When `overlay_bytes` is `Some`, `merge_archived` is called so overlay-
/// overridden nodes are visible in the search.  The returned positions are
/// always into `graph.nodes`; overlay-only nodes (no base counterpart) are
/// excluded because edge traversal in `build_inspect_block` requires a graph
/// index.  Edge traversal for overlay-only nodes is a T7-7 concern.
fn search_nodes<'a>(
    graph: &'a ArchivedZeroCopyGraph,
    overlay_bytes: Option<&[u8]>,
    bare_name: &str,
    owner_filter: Option<&str>,
) -> Vec<(usize, &'a ecp_core::graph::ArchivedNode)> {
    // uid → base index, built once regardless of overlay presence.
    let uid_to_base: rustc_hash::FxHashMap<u64, usize> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.uid.to_native(), i))
        .collect();

    let name_owner_matches = |node: &ecp_core::graph::ArchivedNode, base_idx: usize| -> bool {
        if node.name.resolve(&graph.string_pool) != bare_name {
            return false;
        }
        // Synthetic Annotation nodes (resolver-miss fallback from
        // `decorates_edges`) carry SYNTHETIC_FILE_IDX and have no file:line;
        // they're not meaningful inspect targets.
        if !node.has_owning_file() {
            return false;
        }
        if let Some(owner) = owner_filter {
            return resolve_owner_class(graph, base_idx)
                .map(|oc| oc == owner)
                .unwrap_or(false);
        }
        true
    };

    if let Some(ov_bytes) = overlay_bytes {
        if let Ok(archived_overlay) =
            rkyv::access::<ecp_core::session::ArchivedOverlay, rkyv::rancor::Error>(ov_bytes)
        {
            return merge_archived(graph, archived_overlay)
                .filter_map(|node| {
                    let base_idx = uid_to_base.get(&node.uid.to_native()).copied()?;
                    if name_owner_matches(node, base_idx) {
                        Some((base_idx, &graph.nodes[base_idx]))
                    } else {
                        None
                    }
                })
                .collect();
        }
    }

    // No overlay or corrupt overlay bytes — fall through to base graph only.
    graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(idx, node)| name_owner_matches(node, *idx))
        .collect()
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

    // Load the session overlay (if present) and archive it so we can pass an
    // `&ArchivedOverlay` to `merge_archived`. The overlay bytes live for the
    // rest of this function — the `Option<Vec<u8>>` is the backing store.
    let overlay_bytes: Option<Vec<u8>> = engine
        .overlay_dir()
        .and_then(|dir| load_overlay(dir).ok().flatten())
        .and_then(|ov| rkyv::to_bytes::<rkyv::rancor::Error>(&ov).ok())
        .map(|b| b.into_vec());

    let matching_nodes: Vec<(usize, _)> =
        search_nodes(graph, overlay_bytes.as_deref(), bare_name, owner_filter);

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
    let has_primary_type = matching_nodes.iter().any(|(_, n)| {
        matches!(
            n.kind,
            ecp_core::graph::ArchivedNodeKind::Struct
                | ecp_core::graph::ArchivedNodeKind::Class
                | ecp_core::graph::ArchivedNodeKind::Enum
                | ecp_core::graph::ArchivedNodeKind::Trait
                | ecp_core::graph::ArchivedNodeKind::Interface
        )
    });
    let mut omitted_kinds: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    let matching_nodes: Vec<(usize, _)> = if has_primary_type {
        let mut impl_count: u64 = 0;
        let kept = matching_nodes
            .into_iter()
            .filter(|(_, n)| {
                if matches!(n.kind, ecp_core::graph::ArchivedNodeKind::Impl) {
                    impl_count += 1;
                    false
                } else {
                    true
                }
            })
            .collect();
        if impl_count > 0 {
            omitted_kinds.insert(
                crate::commands::format::kind_to_str(&ecp_core::graph::ArchivedNodeKind::Impl)
                    .to_string(),
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
            graph,
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
        return emit_with_caveat(&result, format, engine.caveat());
    }

    // Ambiguous: return ALL matches as full inspect blocks (not a candidates list).
    let blocks: Vec<serde_json::Value> = matching_nodes
        .iter()
        .map(|(node_idx, _)| {
            build_inspect_block(
                graph,
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
    let result = serde_json::json!({
        "status": "ambiguous",
        "message": format!(
            "Found {} symbols matching '{}'. Use --file_path or --kind to disambiguate.",
            blocks.len(),
            name
        ),
        "matches": blocks,
        "omitted_kinds": omitted_kinds,
    });
    emit_with_caveat(&result, format, engine.caveat())
}
