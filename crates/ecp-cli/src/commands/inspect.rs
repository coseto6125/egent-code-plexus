use crate::commands::format::{node_kind_to_str, rel_type_to_str};
use crate::engine::Engine;
use crate::output::{emit_with_caveat, OutputFormat};
use crate::read_context::{
    ambiguity_candidates, named_declarations, one_type_of_parts, shell_word, target_request,
};
use clap::Args;
use ecp_core::contracts::identity::{resolve_target, select, Ambiguity, TargetResolution};
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

    /// Output format. Absent or `llm`: the LLM-tuned toon payload. `toon`: uncompressed
    /// toon. `json`: full-fidelity JSON. `text`: pretty-printed JSON. Every format except
    /// `json` drops the edge boilerplate.
    #[arg(long)]
    pub format: Option<OutputFormat>,

    /// The kind of the declaration to inspect, one kind only (for example
    /// `function`, `method`, `impl`). The command selects only the
    /// declarations of this kind. A comma-separated list is an error. To keep
    /// only neighbours of some kinds, use `--neighbor-kind`.
    #[arg(long)]
    pub kind: Option<String>,

    /// The file of the declaration to inspect: a path or a part of a path.
    /// The command selects the declarations in the best match: the equal
    /// path, then a path that ends with `/<FILE>`, then a path that holds
    /// `<FILE>`. To keep only neighbours in some files, use `--neighbor-file`.
    #[arg(long = "file", alias = "file_path", alias = "file-path")]
    pub file_path: Option<String>,

    /// Comma-separated node kinds (lowercase, for example `function,method`).
    /// The output keeps only the edges whose other end has one of these
    /// kinds. This flag does not change which declaration the command selects.
    #[arg(long = "neighbor-kind", alias = "neighbor_kind")]
    pub neighbor_kind: Option<String>,

    /// A part of a path (case-sensitive, not a glob). The output keeps only
    /// the edges whose other end is in a file whose path holds this text.
    /// This flag does not change which declaration the command selects.
    #[arg(long = "neighbor-file", alias = "neighbor_file")]
    pub neighbor_file: Option<String>,

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

/// Merged-space node behind an index that came from the target resolver or a
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

/// The sections of an inspect block that map a relation type to its edges.
const EDGE_SECTIONS: [&str; 4] = [
    "incoming",
    "outgoing",
    "heuristic_incoming",
    "heuristic_outgoing",
];

/// Strip constant-across-all-edges boilerplate from an edge-section map for
/// toon/llm output. Operates on the `incoming`, `outgoing`,
/// `heuristic_incoming`, and `heuristic_outgoing` sections of the inspect
/// result — each is a `{rel_type: [{name, kind, filePath, ...}, ...]}` map.
///
/// Fields removed (when constant):
/// - `tier`: when it is the default `"unresolved"` (every edge pre-T4-7);
///   dropping saves ~20 bytes/edge
/// - `checks`: when it is the default `{}`; an edge whose value differs
///   keeps its own copy
/// - `ownerClass`: dropped when every entry across ALL edge sections has
///   `ownerClass: null`; when any entry has a non-null ownerClass the column
///   is kept because the LLM needs it to distinguish method callers
///
/// Affects only the toon/llm path; json format uses the unstripped block.
fn strip_edge_boilerplate(result: &mut serde_json::Value) {
    let section_keys = EDGE_SECTIONS;

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

    let defaults = edge_defaults();
    for &key in &section_keys {
        if let Some(section) = result.get_mut(key).and_then(|s| s.as_object_mut()) {
            for entries in section.values_mut() {
                if let Some(arr) = entries.as_array_mut() {
                    for entry in arr.iter_mut() {
                        if let Some(obj) = entry.as_object_mut() {
                            for field in ["tier", "checks"] {
                                if obj.get(field) == defaults.get(field) {
                                    obj.remove(field);
                                }
                            }
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

/// Per-edge values that never vary today (see `build_inspect_block`). The
/// toon/llm output drops a field that holds its default; an edge whose value
/// differs keeps its own copy.
fn edge_defaults() -> serde_json::Value {
    serde_json::json!({ "tier": "unresolved", "checks": {} })
}

/// Ambiguous result: the candidates only (C1 "Typed target result"). It holds
/// no inspect block and no neighbourhood section, so an agent cannot carry one
/// declaration's callers into a conclusion about another. `total` is the true
/// candidate count; each candidate's `target` selects it alone.
fn ambiguous_result(
    ambiguity: &Ambiguity,
    name: &str,
    omitted_kinds: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    let message = format!(
        "Found {} declarations matching '{}'. This answer lists candidates only and \
         evaluates no neighbours. To inspect one, run `ecp inspect <candidates[].target>`. \
         A candidate with a null target has no target text or filter that selects it \
         alone: read its filePath and line.",
        ambiguity.total(),
        name,
    );
    serde_json::json!({
        "status": "ambiguous",
        "message": message,
        "total": ambiguity.total(),
        "candidates": ambiguity_candidates(ambiguity),
        "omitted_kinds": omitted_kinds,
    })
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

    let defaults = edge_defaults();
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
            "ownerClass": merged.display_owner(edge.target),
            "kind": target_kind,
            "filePath": target_file_path,
            "reason": edge.reason(merged),
            "confidence": edge.confidence(),
            // Mirror `find-schema-bindings`' shape so one agent consuming both
            // commands sees a consistent schema: `tier` is a top-level label,
            // `checks` an object of per-check results. Until T4-7 computes them,
            // tier is the explicit `unresolved` sentinel and checks is empty —
            // both type-stable (T4-7 only fills values, never restructures).
            "tier": defaults["tier"],
            "checks": defaults["checks"],
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
                "ownerClass": merged.display_owner(edge.source),
                "kind": source_kind,
                "filePath": source_file_path,
                "reason": edge.reason(merged),
                "confidence": edge.confidence(),
                // See the outgoing-edge note above: `tier`/`checks` mirror
                // find-schema-bindings; unresolved sentinel + empty object pre-T4-7.
                "tier": defaults["tier"],
                "checks": defaults["checks"],
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
            "ownerClass": merged.display_owner(idx),
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
/// The overlay rebuilds the membership edges of a dirty file
/// (`session::incremental`), so a dirty type lists its current members, a new
/// one included.
///
/// A C# `partial` type lists the members of every part
/// ([`MergedGraph::partial_parts`]), whichever part `idx` is, each once.
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
    let mut parts = merged.partial_parts(idx);
    if parts.is_empty() {
        parts.push(idx);
    }
    let mut seen = rustc_hash::FxHashSet::default();
    for edge in parts.iter().flat_map(|&part| merged.out_edges(part)) {
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
        if !seen.insert(edge.target) {
            continue;
        }
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
///
/// A C# `partial` type reads the in-edges of every part
/// ([`MergedGraph::partial_parts`]), and a part is never its own caller, so
/// either part lists the same callers.
fn bfs_upstream_1hop<F>(
    merged: &MergedGraph<'_>,
    idx: u32,
    constructors: &[u32],
    edge_keeps: &F,
) -> Vec<serde_json::Value>
where
    F: Fn(&str, &str, &str) -> bool,
{
    let mut parts = merged.partial_parts(idx);
    if parts.is_empty() {
        parts.push(idx);
    }
    let mut visited: HashSet<u32> = parts.iter().copied().collect();

    // A Class / Struct is also called through its constructors (see the
    // `incoming` loop in `build_inspect_block`); a caller reaching both counts once.
    let read_from = parts
        .iter()
        .map(|&part| (part, false))
        .chain(constructors.iter().map(|&c| (c, true)));

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

pub fn run(args: InspectArgs, engine: &Engine, _graph_path: &Path) -> Result<(), EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let format = args.format.unwrap_or_default();

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
    // `--kind` and `--file` select the target (C1 "Target request syntax");
    // `--neighbor-kind` and `--neighbor-file` filter its neighbours.
    let request = target_request(name, args.kind.as_deref(), args.file_path.as_deref())
        .map_err(|e| with_neighbor_kind_hint(e, args.kind.as_deref()))?;
    let kind_filter = neighbor_kinds(args.neighbor_kind.as_deref())?;

    // Clean queries must not initialize a view or scan the graph for a delta.
    let view = engine.overlay_dir().and_then(|_| engine.overlay_view());
    let merged = MergedGraph::new(graph, view);

    let mut named = named_declarations(&merged, request.name());
    let mut omitted_kinds: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    // C1 Open point 12: when a primary type (Struct / Class / Enum / Trait /
    // Interface) matches, drop the `Impl` nodes of that name first, so
    // `inspect --name Foo` returns the struct, not "ambiguous". This is the
    // canonical Rust pattern: `struct Foo` + `impl Foo { ... }`. An explicit
    // `--kind` wins: `inspect Foo --kind impl` keeps the `Impl` node.
    if request.kind().is_none() {
        let picked = select(&request, &named);
        let has_primary_type = picked.iter().any(|&i| {
            matches!(
                named[i].kind,
                NodeKind::Struct
                    | NodeKind::Class
                    | NodeKind::Enum
                    | NodeKind::Trait
                    | NodeKind::Interface
            )
        });
        let impl_count = picked
            .iter()
            .filter(|&&i| named[i].kind == NodeKind::Impl)
            .count();
        if has_primary_type && impl_count > 0 {
            named.retain(|c| c.kind != NodeKind::Impl);
            omitted_kinds.insert(
                node_kind_to_str(&NodeKind::Impl).to_string(),
                serde_json::json!(impl_count),
            );
            eprintln!(
                "note: {impl_count} Impl node(s) omitted (primary type matched); use `--kind impl` to inspect one"
            );
        }
    }

    let node_idx = match one_type_of_parts(&merged, resolve_target(&request, &named)) {
        TargetResolution::Unique(c) => c.node,
        TargetResolution::Ambiguous(a) => {
            let result = ambiguous_result(&a, name, omitted_kinds);
            return emit_with_caveat(&result, format, engine.caveat());
        }
        TargetResolution::NotFound { named: n } => {
            let mut message = format!("Symbol '{name}' not found.");
            if n > 0 {
                message.push_str(&format!(
                    " {n} declaration(s) named '{}' exist, but none matches the owner, \
                     @line, --kind or --file of the target. Re-run `ecp inspect {}` to list them.",
                    request.name(),
                    shell_word(request.name()),
                ));
            }
            let result = serde_json::json!({"status": "error", "message": message});
            return emit_with_caveat(&result, format, engine.caveat());
        }
    };

    // Pre-parse the neighbour filters once.
    let rel_filter = parse_csv_lower(args.relation_types.as_deref());
    let file_substr = args
        .neighbor_file
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

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
    if !matches!(format, OutputFormat::Json) {
        strip_edge_boilerplate(&mut result);
    }
    emit_with_caveat(&result, format, engine.caveat())
}

/// `--neighbor-kind` as the lowercase display names the edge filter compares
/// against. An unknown kind is an error: it would empty every section.
fn neighbor_kinds(raw: Option<&str>) -> Result<Option<Vec<String>>, EcpError> {
    parse_csv_lower(raw)
        .map(|kinds| {
            kinds
                .iter()
                .map(|k| {
                    k.parse::<NodeKind>()
                        .map(|kind| kind.as_str().to_ascii_lowercase())
                        .map_err(|()| {
                            EcpError::InvalidArgument(format!(
                                "--neighbor-kind \"{k}\" is not a node kind"
                            ))
                        })
                })
                .collect()
        })
        .transpose()
}

/// `--kind` takes one declaration kind. A list there is most often the old
/// neighbour filter, so the error names the flag that now carries it.
fn with_neighbor_kind_hint(e: EcpError, kind: Option<&str>) -> EcpError {
    match (e, kind) {
        (EcpError::InvalidArgument(msg), Some(k)) if k.contains(',') => {
            EcpError::InvalidArgument(format!(
                "{msg}. To keep only neighbours of these kinds, write `--neighbor-kind {}`",
                shell_word(k.trim()),
            ))
        }
        (e, _) => e,
    }
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

    /// The declarations the resolver lists for `name`, narrowed to `owner`
    /// through the target text `owner.name` when given.
    fn declarations(
        merged: &MergedGraph<'_>,
        name: &str,
        owner: Option<&str>,
    ) -> Vec<(u32, NodeKind)> {
        let named = named_declarations(merged, name);
        let keep: Vec<usize> = match owner {
            Some(o) => {
                let request = target_request(&format!("{o}.{name}"), None, None).unwrap();
                select(&request, &named)
            }
            None => (0..named.len()).collect(),
        };
        keep.into_iter()
            .map(|i| (named[i].node, named[i].kind))
            .collect()
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
            generic_arity: 0,
            flags: None,
        }
    }

    /// The dirty file re-parsed: `Bar.validate` moved to lines 10-14 and now
    /// calls `helper`; `fresh` (16-18) is new and calls `helper`; `doomed`
    /// is gone.
    fn dirty_input() -> OverlayFileInput {
        OverlayFileInput {
            parsed_language: None,
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
    fn test_named_declarations_without_view_returns_base_matches_in_node_order() {
        let bytes = archive(&validate_graph());
        let merged = MergedGraph::new(access(&bytes), None);
        assert_eq!(indices(&declarations(&merged, "validate", None)), [0, 1]);
        assert_eq!(
            indices(&declarations(&merged, "validate", Some("Bar"))),
            [1]
        );
        assert!(declarations(&merged, "missing", None).is_empty());
    }

    /// A namesake in a clean file and one in a dirty file are both listed:
    /// the clean one at its base index, the dirty one at its virtual index.
    #[test]
    fn test_named_declarations_clean_and_dirty_namesakes_lists_both() {
        with_dirty_view(|merged| {
            let dirty = virtual_idx(&merged, "validate");
            assert_eq!(
                declarations(&merged, "validate", None),
                [(2, NodeKind::Method), (dirty, NodeKind::Method)]
            );
        });
    }

    #[test]
    fn test_named_declarations_owner_filter_on_virtual_method_matches_virtual() {
        with_dirty_view(|merged| {
            let dirty = virtual_idx(&merged, "validate");
            assert_eq!(
                indices(&declarations(&merged, "validate", Some("Bar"))),
                [dirty]
            );
            assert_eq!(
                indices(&declarations(&merged, "validate", Some("Foo"))),
                [2]
            );
            assert!(declarations(&merged, "validate", Some("Nobody")).is_empty());
        });
    }

    #[test]
    fn test_named_declarations_deleted_dirty_symbol_not_found() {
        with_dirty_view(|merged| {
            assert!(declarations(&merged, "doomed", None).is_empty());
        });
    }

    #[test]
    fn test_named_declarations_overlay_only_symbol_found() {
        with_dirty_view(|merged| {
            let fresh = virtual_idx(&merged, "fresh");
            assert_eq!(
                declarations(&merged, "fresh", None),
                [(fresh, NodeKind::Function)]
            );
        });
    }

    /// A dirty file that defines the function `doomed` twice (both branches
    /// of an `if`) yields two virtual nodes under the base symbol's uid. The
    /// index keeps the first. Methods differ: a later one is an overload with
    /// its own uid, and both stay listed.
    #[test]
    fn test_named_declarations_same_uid_redefinition_lists_first_only() {
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
        assert_eq!(indices(&declarations(&merged, "doomed", None)), [first]);
    }

    /// Rejected or corrupt fragments leave no accepted input, and an empty
    /// input set builds no view: the answer is the base graph's, deleted
    /// symbol included.
    #[test]
    fn test_named_declarations_rejected_fragment_answers_from_base() {
        let bytes = archive(&dirty_graph());
        let graph = access(&bytes);
        let view = OverlayView::build(graph, &[]);
        assert!(view.is_none());
        let merged = MergedGraph::new(graph, view.as_ref());
        assert_eq!(indices(&declarations(&merged, "validate", None)), [1, 2]);
        assert_eq!(indices(&declarations(&merged, "doomed", None)), [3]);
        assert!(declarations(&merged, "fresh", None).is_empty());
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

    /// The `--neighbor-file` filter applies to a virtual node's edges like any other.
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
            parsed_language: None,
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
    fn test_named_declarations_with_index_matches_full_scan() {
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
                    declarations(&fast, name, owner),
                    declarations(&slow, name, owner),
                    "{name:?} owner={owner:?} view={}",
                    fast_view.is_some()
                );
            }
        }
    }

    #[test]
    fn test_named_declarations_shared_name_lists_every_node_ascending() {
        let bytes = archive(&same_name_graph());
        let merged = MergedGraph::new(access(&bytes), None);
        let hits = indices(&declarations(&merged, "shared", None));
        assert_eq!(hits.len(), 1200);
        assert!(hits.windows(2).all(|w| w[0] < w[1]), "node order");
    }

    /// The tombstone has an empty name but still owns a file: only the scan
    /// matches it, so an empty bare name must keep the scan.
    #[test]
    fn test_named_declarations_empty_bare_name_keeps_tombstone_matches() {
        let bytes = archive(&same_name_graph());
        let merged = MergedGraph::new(access(&bytes), None);
        assert_eq!(declarations(&merged, "", None).len(), 12);
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

    /// The ambiguity of the bare name `validate` in `validate_graph`.
    fn validate_ambiguity() -> Ambiguity {
        let bytes = archive(&validate_graph());
        let merged = MergedGraph::new(access(&bytes), None);
        let request = target_request("validate", None, None).unwrap();
        match resolve_target(&request, &named_declarations(&merged, "validate")) {
            TargetResolution::Ambiguous(a) => a,
            other => panic!("expected an ambiguity, got {other:?}"),
        }
    }

    /// Stands for: the hint names the field that selects one candidate
    /// (`candidates[].target`) and says that neighbours were not evaluated,
    /// so an absent section never reads as "no callers".
    #[test]
    fn test_ambiguous_result_hint_points_at_candidate_targets() {
        let r = ambiguous_result(&validate_ambiguity(), "validate", Default::default());
        let msg = r["message"].as_str().unwrap();
        assert!(msg.contains("candidates[].target"), "{msg}");
        assert!(msg.contains("evaluates no neighbours"), "{msg}");
        assert!(!msg.contains("matches[]"), "{msg}");
        assert!(!msg.contains("omits is empty"), "{msg}");
    }

    /// Stands for: C1 "Typed target result" for inspect. The payload is the
    /// status, the true count and one re-runnable target per candidate; it
    /// holds no inspect block and no neighbourhood section.
    #[test]
    fn test_ambiguous_result_lists_candidates_without_blocks() {
        let r = ambiguous_result(&validate_ambiguity(), "validate", Default::default());
        let mut keys: Vec<&str> = r.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["candidates", "message", "omitted_kinds", "status", "total"]
        );
        assert_eq!(r["status"], "ambiguous");
        assert_eq!(r["total"], 2);
        let facts: Vec<(&str, &str, &str)> = r["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["filePath"].as_str().unwrap(),
                    c["kind"].as_str().unwrap(),
                    c["target"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            facts,
            [
                ("src/foo.py", "Method", "Foo.validate"),
                ("src/bar.py", "Method", "Bar.validate"),
            ]
        );
    }

    /// Stands for: the toon/llm compression drops only the default `tier` and
    /// `checks` of an edge. A non-default value, and the symbol's owner,
    /// reach the reader of the one selected declaration.
    #[test]
    fn test_strip_edge_boilerplate_keeps_non_default_edge_values() {
        let mut block = block_with_edge("Foo");
        strip_edge_boilerplate(&mut block);
        let calls = &block["incoming"]["calls"];
        assert!(calls[0].get("tier").is_none() && calls[0].get("checks").is_none());
        assert_eq!(calls[1]["tier"], "verified");
        assert_eq!(calls[1]["checks"], serde_json::json!({"x": true}));
        assert_eq!(block["symbol"]["ownerClass"], "Foo");
        assert_eq!(edge_defaults()["tier"], "unresolved");
    }

    #[test]
    fn test_edge_sections_are_the_keys_stripping_visits() {
        let mut block = serde_json::json!({});
        for key in EDGE_SECTIONS {
            block[key] =
                serde_json::json!({"calls": [{"name": "c", "tier": "unresolved", "checks": {}}]});
        }
        strip_edge_boilerplate(&mut block);
        for key in EDGE_SECTIONS {
            assert!(block[key]["calls"][0].get("tier").is_none(), "{key}");
            assert!(block[key]["calls"][0].get("checks").is_none(), "{key}");
        }
    }

    /// Stands for: a list in `--kind` is the old neighbour filter. The error
    /// names `--neighbor-kind` so the agent moves the list there.
    /// Stands for: `--neighbor-kind` accepts every spelling `NodeKind`
    /// parses and compares against the display name, so `enum_variant`
    /// keeps EnumVariant neighbours; an unknown kind fails instead of
    /// emptying every section.
    #[test]
    fn test_neighbor_kinds_parses_spellings_and_rejects_unknown() {
        assert_eq!(
            neighbor_kinds(Some("enum_variant, Function")).unwrap(),
            Some(vec!["enumvariant".to_string(), "function".to_string()])
        );
        assert_eq!(neighbor_kinds(Some(" , ")).unwrap(), None);
        assert_eq!(neighbor_kinds(None).unwrap(), None);
        let err = neighbor_kinds(Some("function,banana"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("\"banana\" is not a node kind"), "{err}");
    }

    #[test]
    fn test_with_neighbor_kind_hint_list_kind_names_neighbor_flag() {
        let kind = Some("function,method");
        let e = target_request("caller", kind, None).unwrap_err();
        let msg = with_neighbor_kind_hint(e, kind).to_string();
        assert!(msg.contains("--neighbor-kind function,method"), "{msg}");
        let e = target_request("caller", Some("banana"), None).unwrap_err();
        let msg = with_neighbor_kind_hint(e, Some("banana")).to_string();
        assert!(!msg.contains("--neighbor-kind"), "{msg}");
        assert!(msg.contains("banana"), "{msg}");
    }

    /// nodes: 0 Reader (Reader.cs, the representative) · 1 Reader
    /// (Reader.Async.cs) · 2 Read · 3 ReadAsync · 4 TextReader
    /// edges: 1 →PartOf→ 0, TextReader →Extends→ 0, HasMethod 0→Read,
    /// 1→ReadAsync, and a second HasMethod 0→ReadAsync, so one member has
    /// two membership edges.
    fn partial_type_graph() -> ZeroCopyGraph {
        let mut fx = GraphFixture::new();
        let main = fx.node(NodeKind::Class, "Lib/Reader.cs", "Reader");
        let async_part = fx.node(NodeKind::Class, "Lib/Reader.Async.cs", "Reader");
        let read = fx.method("Lib/Reader.cs", "Reader", "Read");
        let read_async = fx.method("Lib/Reader.Async.cs", "Reader", "ReadAsync");
        let text_reader = fx.node(NodeKind::Class, "Lib/TextReader.cs", "TextReader");
        fx.edge(async_part, main, RelType::PartOf);
        fx.edge(text_reader, main, RelType::Extends);
        fx.edge(main, read, RelType::HasMethod);
        fx.edge(async_part, read_async, RelType::HasMethod);
        fx.edge(main, read_async, RelType::HasMethod);
        fx.build()
    }

    /// Stands for: inspecting either part of a C# partial type lists the
    /// members of every part, each once, and the same direct callers: the
    /// subclass, never the other part. Before the fix each part listed only
    /// its own members, so the representative that `Extends` reaches hid
    /// the async part's methods, and the async part listed no caller.
    #[test]
    fn test_collect_contained_members_partial_type_lists_every_part_once() {
        let bytes = archive(&partial_type_graph());
        let merged = MergedGraph::new(access(&bytes), None);
        for part in [0, 1] {
            let inspected = block(&merged, part);
            let mut methods: Vec<&str> = inspected["contained_methods"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["name"].as_str().unwrap())
                .collect();
            methods.sort_unstable();
            assert_eq!(methods, ["Read", "ReadAsync"], "part {part}");
            assert_eq!(upstream_names(&inspected), ["TextReader"], "part {part}");
        }
    }
}
