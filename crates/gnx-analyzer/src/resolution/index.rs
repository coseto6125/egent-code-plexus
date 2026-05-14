use gnx_core::graph::NodeKind;
use std::collections::HashMap;

pub type NodeId = u32;

/// Edge kinds the resolver resolves towards. Constrains Tier-3 fallback so a
/// bare callee like `format` never resolves to a Variable/Const that happens
/// to share the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveTarget {
    Callable,
    Type,
    Any,
}

/// A high-performance global symbol index mapping node names and file locations
/// to their corresponding globally unique node IDs.
#[derive(Debug, Default)]
pub struct SymbolTable {
    /// Maps `file_path` -> `node_name` -> `node_id`.
    ///
    /// Using a nested HashMap allows us to look up symbols by `&str` without
    /// needing to allocate a `(String, String)` tuple just for the query key.
    /// This provides fast O(1) lookups for `SameFile` and `ImportScoped` resolution.
    file_scoped: HashMap<String, HashMap<String, u32>>,

    /// Maps a `node_name` to a list of node IDs across all files.
    ///
    /// Tier-3 (Global) fallback consults this list, then narrows by
    /// `node_kinds[id]` to match the requested `ResolveTarget`.
    global_scoped: HashMap<String, Vec<u32>>,

    /// Reverse map `node_id` → owning `file_path`. Populated by
    /// `register_node` alongside the other indexes; consumed by the resolver
    /// decision dump to report the resolved target file.
    id_to_file: HashMap<u32, String>,

    /// Kind per node, indexed by `node_id`. Populated during build by
    /// `register_node` in monotonic-id order; consulted by
    /// `lookup_unique_global` to filter candidates without allocating side
    /// sets. Lives only during build — the finalized `ZeroCopyGraph.nodes[id].kind`
    /// is the steady-state source of truth.
    node_kinds: Vec<NodeKind>,
}

impl SymbolTable {
    /// Creates a new empty `SymbolTable`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a node with the given file path, node name, node ID, and kind.
    ///
    /// `node_id` must be the monotonic sequential index assigned by the builder
    /// (debug-asserted), so `node_kinds[id]` indexing works in
    /// `lookup_unique_global`.
    pub fn register_node(
        &mut self,
        file_path: &str,
        node_name: &str,
        node_id: u32,
        kind: NodeKind,
    ) {
        debug_assert_eq!(
            node_id as usize,
            self.node_kinds.len(),
            "register_node ids must be monotonic and dense for kind-indexing"
        );
        self.file_scoped
            .entry(file_path.to_string())
            .or_default()
            .insert(node_name.to_string(), node_id);

        self.global_scoped
            .entry(node_name.to_string())
            .or_default()
            .push(node_id);

        // Reverse map for dump-side lookup
        self.id_to_file.insert(node_id, file_path.to_string());

        self.node_kinds.push(kind);
    }

    /// Looks up a node ID by its file path and node name.
    ///
    /// Returns `Some(node_id)` if found, or `None` if the symbol doesn't exist
    /// in the specified file.
    pub fn lookup_in_file(&self, file_path: &str, node_name: &str) -> Option<u32> {
        self.file_scoped
            .get(file_path)
            .and_then(|file_map| file_map.get(node_name).copied())
    }

    /// Tier-3 global lookup: returns the single node id matching `name` whose
    /// kind satisfies `target`, or `None` if zero or ≥2 candidates remain.
    ///
    /// Refusing to guess when ambiguous is the dominant defence against
    /// bare-name fan-out (`new`, `format`, `default`, `main`, ...).
    /// Short-circuits on the second match without allocating an intermediate Vec.
    pub fn lookup_unique_global(&self, node_name: &str, target: ResolveTarget) -> Option<u32> {
        let raw = self.global_scoped.get(node_name)?;
        let predicate: fn(NodeKind) -> bool = match target {
            ResolveTarget::Any => return (raw.len() == 1).then(|| raw[0]),
            ResolveTarget::Callable => NodeKind::is_callable,
            ResolveTarget::Type => NodeKind::is_type,
        };
        let mut found = None;
        for &id in raw {
            if predicate(self.node_kinds[id as usize]) {
                if found.is_some() {
                    return None;
                }
                found = Some(id);
            }
        }
        found
    }

    /// Total count of same-named candidates (before kind filter). Exposed for
    /// the resolver decision dump's `alt_count` telemetry.
    pub fn global_match_count(&self, node_name: &str) -> u32 {
        self.global_scoped
            .get(node_name)
            .map(|v| v.len() as u32)
            .unwrap_or(0)
    }

    /// Reverse lookup: given a `node_id`, return its owning file path. Used by
    /// the resolver decision dump to materialize `target_file` in JSONL output.
    pub fn file_of(&self, node_id: u32) -> Option<&str> {
        self.id_to_file.get(&node_id).map(|s| s.as_str())
    }
}
