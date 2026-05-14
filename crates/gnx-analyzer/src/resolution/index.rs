use std::collections::HashMap;

pub type NodeId = u32;

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
    /// This is used for `ResolutionTier::Global` lookups where we need to find
    /// all possible candidates for a given symbol name.
    global_scoped: HashMap<String, Vec<u32>>,

    /// Reverse map `node_id` → owning `file_path`. Populated by
    /// `register_node` alongside the other indexes; consumed by the resolver
    /// decision dump to report the resolved target file.
    id_to_file: HashMap<u32, String>,
}

impl SymbolTable {
    /// Creates a new empty `SymbolTable`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a node with the given file path, node name, and node ID.
    pub fn register_node(&mut self, file_path: &str, node_name: &str, node_id: u32) {
        // Register in the file-scoped map
        self.file_scoped
            .entry(file_path.to_string())
            .or_default()
            .insert(node_name.to_string(), node_id);

        // Register in the global-scoped map
        self.global_scoped
            .entry(node_name.to_string())
            .or_default()
            .push(node_id);

        // Register reverse map for dump-side lookup
        self.id_to_file.insert(node_id, file_path.to_string());
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

    /// Looks up all node IDs that share the given global node name.
    ///
    /// Returns a list of matching `node_id`s, or an empty list if none are found.
    pub fn lookup_global(&self, node_name: &str) -> Vec<u32> {
        self.global_scoped
            .get(node_name)
            .cloned()
            .unwrap_or_default()
    }

    /// Reverse lookup: given a `node_id`, return its owning file path. Used by
    /// the resolver decision dump to materialize `target_file` in JSONL output.
    pub fn file_of(&self, node_id: u32) -> Option<&str> {
        self.id_to_file.get(&node_id).map(|s| s.as_str())
    }
}
