use graph_nexus_core::graph::NodeKind;
use rustc_hash::FxHashMap;

pub type NodeId = u32;

/// Edge kinds the resolver resolves towards. Constrains Tier-3 fallback so a
/// bare callee like `format` never resolves to a Variable/Const that happens
/// to share the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveTarget {
    Callable,
    Type,
}

/// Per-parser-provider language tag. One variant per registered analyzer
/// provider; `from_path` performs the lookup by file extension (multi-ext
/// providers like JavaScript / TypeScript fold to a single variant).
///
/// Used by `lookup_unique_global` as a Tier-3 caller-vs-target barrier:
/// bare callee names never cross language boundaries (a Rust `result.is_some()`
/// never resolves to a vendored Move test fixture's `is_some` function).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    #[default]
    Unknown,
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Java,
    Kotlin,
    Go,
    Ruby,
    Php,
    CSharp,
    Swift,
    Dart,
    Solidity,
    Sql,
    C,
    Cpp,
    Move,
    Nim,
    Cairo,
    Vyper,
    Verilog,
    Hcl,
    Crystal,
    Lua,
    Zig,
    Bash,
    Dockerfile,
    DockerCompose,
    GitHubActions,
    Yaml,
    Markdown,
}

impl Language {
    /// Map a repo-relative file path to its provider language. Mirrors the
    /// extension routing in `commands/analyze.rs` plus path-based overrides
    /// for `Dockerfile` / `docker-compose.{yml,yaml}` / `.github/workflows/*`.
    pub fn from_path(path: &str) -> Self {
        let normalized = path.replace('\\', "/");
        let basename = normalized.rsplit('/').next().unwrap_or("");

        // Path / basename overrides before extension routing.
        if matches!(basename, "Dockerfile" | "dockerfile") {
            return Self::Dockerfile;
        }
        if matches!(
            basename,
            "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml"
        ) {
            return Self::DockerCompose;
        }
        let ext = basename.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        if matches!(ext, "yml" | "yaml")
            && (normalized.contains("/.github/workflows/")
                || normalized.starts_with(".github/workflows/"))
        {
            return Self::GitHubActions;
        }

        match ext {
            "rs" => Self::Rust,
            "py" | "pyi" => Self::Python,
            "ts" | "tsx" => Self::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "java" => Self::Java,
            "kt" | "kts" => Self::Kotlin,
            "go" => Self::Go,
            "rb" => Self::Ruby,
            "php" => Self::Php,
            "cs" => Self::CSharp,
            "swift" => Self::Swift,
            "dart" => Self::Dart,
            "sol" => Self::Solidity,
            "sql" => Self::Sql,
            "c" | "h" => Self::C,
            "cpp" | "hpp" | "cc" | "hh" | "cxx" | "hxx" => Self::Cpp,
            "move" => Self::Move,
            "nim" => Self::Nim,
            "cairo" => Self::Cairo,
            "vy" => Self::Vyper,
            "v" | "sv" | "vh" | "svh" => Self::Verilog,
            "tf" | "tfvars" | "hcl" => Self::Hcl,
            "cr" => Self::Crystal,
            "lua" | "luau" => Self::Lua,
            "zig" => Self::Zig,
            "sh" | "bash" => Self::Bash,
            "yml" | "yaml" => Self::Yaml,
            "md" | "txt" | "rst" => Self::Markdown,
            _ => Self::Unknown,
        }
    }

    /// Canonical display name. Used by `gnx search` to emit the `language`
    /// field on each Hit. Overrides Debug formatting for `Php` ("PHP") and
    /// `Cpp` ("C++") where the conventional name differs from the variant
    /// identifier.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Rust => "Rust",
            Self::Python => "Python",
            Self::TypeScript => "TypeScript",
            Self::JavaScript => "JavaScript",
            Self::Java => "Java",
            Self::Kotlin => "Kotlin",
            Self::Go => "Go",
            Self::Ruby => "Ruby",
            Self::Php => "PHP",
            Self::CSharp => "CSharp",
            Self::Swift => "Swift",
            Self::Dart => "Dart",
            Self::Solidity => "Solidity",
            Self::Sql => "SQL",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::Move => "Move",
            Self::Nim => "Nim",
            Self::Cairo => "Cairo",
            Self::Vyper => "Vyper",
            Self::Verilog => "Verilog",
            Self::Hcl => "HCL",
            Self::Crystal => "Crystal",
            Self::Lua => "Lua",
            Self::Zig => "Zig",
            Self::Bash => "Bash",
            Self::Dockerfile => "Dockerfile",
            Self::DockerCompose => "DockerCompose",
            Self::GitHubActions => "GitHubActions",
            Self::Yaml => "YAML",
            Self::Markdown => "Markdown",
        }
    }
}

/// Build-time metadata about a file, cached per node id so `lookup_unique_global`
/// can apply caller-vs-candidate barriers (language match + vendor isolation)
/// without re-parsing the path on every Tier-3 probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileMeta {
    /// Path contains a `/vendor/` segment. Non-vendor callers must not resolve
    /// to vendor targets — vendor grammar test corpora share short common names
    /// (`is_some`, `get`, `new`) with stdlib methods, producing one false edge
    /// per call site that survives the kind+unique filter.
    pub is_vendor: bool,
    pub language: Language,
}

impl FileMeta {
    pub fn from_path(path: &str) -> Self {
        let normalized = path.replace('\\', "/");
        Self {
            is_vendor: normalized.contains("/vendor/") || normalized.starts_with("vendor/"),
            language: Language::from_path(path),
        }
    }
}

/// A high-performance global symbol index mapping node names and file locations
/// to their corresponding globally unique node IDs.
#[derive(Debug, Default)]
pub struct SymbolTable {
    /// Maps `file_path` -> `node_name` -> `node_id`.
    ///
    /// Using a nested map allows us to look up symbols by `&str` without
    /// needing to allocate a `(String, String)` tuple just for the query key.
    /// `FxHashMap` here: keys are short strings (file paths, identifier
    /// names) where SipHash's avalanche guarantees aren't useful — FxHash
    /// is ~5x faster on this distribution and Build Pass 1's `register_node`
    /// is hot (~14k × 3 inserts on `.sample_repo`).
    file_scoped: FxHashMap<String, FxHashMap<String, u32>>,

    /// Maps a `node_name` to a list of node IDs across all files.
    ///
    /// Tier-3 (Global) fallback consults this list, then narrows by
    /// `node_kinds[id]` to match the requested `ResolveTarget`.
    global_scoped: FxHashMap<String, Vec<u32>>,

    /// Reverse map `node_id` → owning `file_path`. Populated by
    /// `register_node` alongside the other indexes; consumed by the resolver
    /// decision dump to report the resolved target file.
    id_to_file: FxHashMap<u32, String>,

    /// Kind per node, indexed by `node_id`. Populated during build by
    /// `register_node` in monotonic-id order; consulted by
    /// `lookup_unique_global` to filter candidates without allocating side
    /// sets. Lives only during build — the finalized `ZeroCopyGraph.nodes[id].kind`
    /// is the steady-state source of truth.
    node_kinds: Vec<NodeKind>,

    /// File metadata per node (language + vendor flag). Cached so the Tier-3
    /// barrier check is O(1) per candidate. Parallel-indexed with `node_kinds`.
    node_file_meta: Vec<FileMeta>,
}

impl SymbolTable {
    /// Creates a new empty `SymbolTable`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a node with the given file path, node name, node ID, and kind.
    ///
    /// `node_id` must be the monotonic sequential index assigned by the builder
    /// (debug-asserted), so `node_kinds[id]` / `node_file_meta[id]` indexing
    /// works in `lookup_unique_global`.
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
        self.node_file_meta.push(FileMeta::from_path(file_path));
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
    /// kind satisfies `target` AND whose file meta is reachable from the
    /// caller (same language + non-vendor unless caller is also vendor), or
    /// `None` if zero or ≥2 such candidates remain.
    ///
    /// Layered defences against bare-name fan-out:
    ///   * Kind filter — Callable/Type narrowing.
    ///   * Language barrier — Rust caller never resolves to a Move target.
    ///   * Vendor barrier — non-vendor caller never reaches into vendor corpus.
    ///   * Uniqueness — refuse to guess when ≥2 candidates remain post-filter.
    ///
    /// Short-circuits on the second matching candidate without allocating an
    /// intermediate Vec.
    pub fn lookup_unique_global(
        &self,
        node_name: &str,
        target: ResolveTarget,
        caller: FileMeta,
    ) -> Option<u32> {
        let raw = self.global_scoped.get(node_name)?;
        let predicate: fn(NodeKind) -> bool = match target {
            ResolveTarget::Callable => NodeKind::is_callable,
            ResolveTarget::Type => NodeKind::is_type,
        };
        let mut found = None;
        for &id in raw {
            let cand = self.node_file_meta[id as usize];
            if cand.language != caller.language {
                continue;
            }
            if cand.is_vendor && !caller.is_vendor {
                continue;
            }
            if !predicate(self.node_kinds[id as usize]) {
                continue;
            }
            if found.is_some() {
                return None;
            }
            found = Some(id);
        }
        found
    }

    /// Total count of same-named candidates (before kind/locality filters).
    /// Exposed for the resolver decision dump's `alt_count` telemetry.
    pub fn global_match_count(&self, node_name: &str) -> u32 {
        self.global_scoped
            .get(node_name)
            .map(|v| v.len() as u32)
            .unwrap_or(0)
    }

    /// Post-filter candidate count for the same predicate chain that
    /// `lookup_unique_global` walks (language barrier, vendor barrier,
    /// kind filter). Called on lookup miss only — the hot Tier-3 path
    /// still short-circuits via `lookup_unique_global` and only invokes
    /// this when it needs to tell `AmbiguousGlobal` from `Unresolved`.
    pub fn count_global_kind_filtered(
        &self,
        node_name: &str,
        target: ResolveTarget,
        caller: FileMeta,
    ) -> u32 {
        let Some(raw) = self.global_scoped.get(node_name) else {
            return 0;
        };
        let predicate: fn(NodeKind) -> bool = match target {
            ResolveTarget::Callable => NodeKind::is_callable,
            ResolveTarget::Type => NodeKind::is_type,
        };
        let mut count = 0u32;
        for &id in raw {
            let cand = self.node_file_meta[id as usize];
            if cand.language != caller.language {
                continue;
            }
            if cand.is_vendor && !caller.is_vendor {
                continue;
            }
            if !predicate(self.node_kinds[id as usize]) {
                continue;
            }
            count += 1;
        }
        count
    }

    /// Reverse lookup: given a `node_id`, return its owning file path. Used by
    /// the resolver decision dump to materialize `target_file` in JSONL output.
    pub fn file_of(&self, node_id: u32) -> Option<&str> {
        self.id_to_file.get(&node_id).map(|s| s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_from_path_handles_multi_ext_providers() {
        assert_eq!(Language::from_path("a/b.rs"), Language::Rust);
        assert_eq!(Language::from_path("a/b.py"), Language::Python);
        assert_eq!(Language::from_path("a/b.pyi"), Language::Python);
        assert_eq!(Language::from_path("a/b.ts"), Language::TypeScript);
        assert_eq!(Language::from_path("a/b.tsx"), Language::TypeScript);
        assert_eq!(Language::from_path("a/b.js"), Language::JavaScript);
        assert_eq!(Language::from_path("a/b.mjs"), Language::JavaScript);
        assert_eq!(Language::from_path("a/b.h"), Language::C);
        assert_eq!(Language::from_path("a/b.hpp"), Language::Cpp);
        assert_eq!(Language::from_path("a/b.move"), Language::Move);
    }

    #[test]
    fn language_from_path_handles_path_based_routing() {
        assert_eq!(Language::from_path("any/Dockerfile"), Language::Dockerfile);
        assert_eq!(
            Language::from_path("svc/docker-compose.yml"),
            Language::DockerCompose
        );
        assert_eq!(
            Language::from_path(".github/workflows/ci.yml"),
            Language::GitHubActions
        );
        // Plain yml outside .github/workflows stays as Yaml
        assert_eq!(Language::from_path("config/app.yml"), Language::Yaml);
    }

    #[test]
    fn file_meta_detects_vendor_segment() {
        assert!(FileMeta::from_path("crates/vendor/tree-sitter-move/x.move").is_vendor);
        assert!(FileMeta::from_path("vendor/x.move").is_vendor);
        assert!(!FileMeta::from_path("crates/graph-nexus-analyzer/src/x.rs").is_vendor);
        assert!(!FileMeta::from_path("src/vendored_helper.rs").is_vendor);
    }
}
