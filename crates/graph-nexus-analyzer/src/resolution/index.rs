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
        if path.contains('\\') {
            let normalized = path.replace('\\', "/");
            Self::from_normalized_path(&normalized)
        } else {
            Self::from_normalized_path(path)
        }
    }

    /// Fast path for callers that have already converted backslashes (Pass 1
    /// in `builder.rs` and most repo-rooted paths on Linux/macOS). Skips the
    /// `replace('\\','/')` allocation entirely.
    pub fn from_normalized_path(path: &str) -> Self {
        let normalized = path;
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
            "c" => Self::C,
            // `.h` routes to C++ (matches ref-gitnexus dispatch). `.h` is genuinely
            // ambiguous — C headers and C++ headers share the extension — but C++
            // parsing is a near-superset of C, while C parsing produces ERROR
            // nodes on any C++-only construct (class, template, namespace, &,
            // operator overload). Real codebases ship C++ libraries with `.h`
            // headers (nlohmann/json, doctest, LLVM Fuzzer, Catch2, …); routing
            // them to the C parser silently drops every class/method/template.
            "cpp" | "hpp" | "cc" | "hh" | "cxx" | "hxx" | "h" => Self::Cpp,
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

    /// Canonical display name. Used by `gnx find --mode bm25` to emit the `language`
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
        if path.contains('\\') {
            let normalized = path.replace('\\', "/");
            Self::from_normalized_path(&normalized)
        } else {
            Self::from_normalized_path(path)
        }
    }

    /// Fast path for callers that have already normalised separators. Skips
    /// the `replace('\\','/')` allocation (one per call) — meaningful in Pass
    /// 1 where this is called per node (~300k on `.sample_repo`).
    pub fn from_normalized_path(path: &str) -> Self {
        Self {
            is_vendor: path.contains("/vendor/") || path.starts_with("vendor/"),
            language: Language::from_normalized_path(path),
        }
    }
}

/// A high-performance global symbol index mapping node names and file locations
/// to their corresponding globally unique node IDs.
#[derive(Debug, Default)]
pub struct SymbolTable {
    /// Maps `file_path` -> `node_name` -> `Vec<node_id>`.
    ///
    /// Multi-id-per-name (was single u32 prior to PR #71 round 3): a file
    /// can hold two same-name nodes of different kinds — e.g. C#'s inner
    /// class `Foo` next to property `Foo`, Java's `class Foo { Foo() }`
    /// constructor sharing the class name, Kotlin's property + accessor
    /// pair both keyed `samples`. The previous `HashMap<name, id>` was
    /// last-write-wins, so resolver Tier-1 SameFile lookup would return
    /// the second-registered node regardless of whether the call site
    /// wanted a Callable or a Type — producing `Constructor -> Property`
    /// edges that are syntactically nonsense. Storing all node_ids and
    /// filtering at lookup time via `ResolveTarget` predicate fixes that.
    ///
    /// `FxHashMap` here: keys are short strings (file paths, identifier
    /// names) where SipHash's avalanche guarantees aren't useful — FxHash
    /// is ~5x faster on this distribution and Build Pass 1's `register_node`
    /// is hot (~14k × 3 inserts on `.sample_repo`).
    file_scoped: FxHashMap<String, FxHashMap<String, Vec<u32>>>,

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

    /// Basename-stem → file paths sharing that stem. Populated once after
    /// Pass 1 via [`SymbolTable::build_stem_index`]; the resolver's Tier-4
    /// module-file fallback reads it via [`SymbolTable::files_by_stem`].
    ///
    /// Without this index, Tier 4 would scan every `file_scoped.keys()`
    /// per failed-qualifier resolution (~3 k entries on the gitnexus-rs
    /// index, fires once per unresolved qualified call → millions of
    /// stem comparisons on cold-index build). The map collapses that to
    /// an O(1) lookup + O(candidates-per-stem) inner walk.
    stem_index: FxHashMap<String, Vec<String>>,
}

impl SymbolTable {
    /// Creates a new empty `SymbolTable`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Populate the `stem_index` from the file paths already in
    /// `file_scoped`. Call exactly once after Pass 1 finishes registering
    /// nodes, before any resolver tier reads from the index. Idempotent
    /// (clears before rebuild) so a future caller adding files post-Pass-1
    /// can re-finalize without leaking stale entries.
    pub fn build_stem_index(&mut self) {
        self.stem_index.clear();
        for path in self.file_scoped.keys() {
            let Some(stem) = std::path::Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
            else {
                continue;
            };
            self.stem_index
                .entry(stem.to_string())
                .or_default()
                .push(path.clone());
        }
    }

    /// O(1) lookup of file paths whose basename stem equals `stem`.
    /// Returns an empty slice when the stem has no match or
    /// [`SymbolTable::build_stem_index`] hasn't been called. The resolver
    /// tiers that consume this all fire after the builder has finalized
    /// the index, so an empty slice in production means "no match" rather
    /// than "index not built".
    pub fn files_by_stem(&self, stem: &str) -> &[String] {
        self.stem_index
            .get(stem)
            .map(Vec::as_slice)
            .unwrap_or(&[])
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
        // Primary-type priority: when a non-Impl node lands for a name that
        // previously only had Impl entries (or is brand-new), we push it and
        // remove any prior Impl placeholder. When an Impl node lands for a name
        // that already has a non-Impl entry, we skip the Impl — Pass-2
        // class_membership must resolve "Foo" to the Struct, not the impl block.
        let file_map = self.file_scoped.entry(file_path.to_string()).or_default();
        let entry = file_map.entry(node_name.to_string()).or_default();
        if kind == NodeKind::Impl {
            let has_primary = entry.iter().any(|&id| {
                !matches!(
                    self.node_kinds.get(id as usize),
                    Some(NodeKind::Impl)
                )
            });
            if !has_primary {
                entry.push(node_id);
            }
        } else {
            // Non-Impl: remove any prior Impl-only placeholders, then push.
            entry.retain(|&id| {
                !matches!(self.node_kinds.get(id as usize), Some(NodeKind::Impl))
            });
            entry.push(node_id);
        }

        self.global_scoped
            .entry(node_name.to_string())
            .or_default()
            .push(node_id);

        // Reverse map for dump-side lookup
        self.id_to_file.insert(node_id, file_path.to_string());

        self.node_kinds.push(kind);
        self.node_file_meta.push(FileMeta::from_path(file_path));
    }

    /// Hot-path variant of `register_node` for callers that already
    /// computed `FileMeta` for this file (i.e. Pass 1 hoists `FileMeta`
    /// once per file out of the per-node loop, since 1 file ↔ ~25 nodes
    /// on `.sample_repo` and `FileMeta::from_path` allocates one `String`
    /// per call for the `\\` → `/` replace). Semantically identical to
    /// `register_node` but skips the redundant per-node path parse.
    ///
    /// Map inserts use `get_mut` → fall-through `entry(.to_string())` so
    /// the file_path / node_name keys only allocate on first sight. After
    /// node #1 of a file lands, the next ~24 nodes hit the get_mut fast
    /// path and reuse the existing key bucket.
    pub fn register_node_with_meta(
        &mut self,
        file_path: &str,
        file_meta: FileMeta,
        node_name: &str,
        node_id: u32,
        kind: NodeKind,
    ) {
        debug_assert_eq!(
            node_id as usize,
            self.node_kinds.len(),
            "register_node ids must be monotonic and dense for kind-indexing"
        );

        if let Some(file_map) = self.file_scoped.get_mut(file_path) {
            file_map
                .entry(node_name.to_string())
                .or_default()
                .push(node_id);
        } else {
            let mut m = FxHashMap::default();
            m.insert(node_name.to_string(), vec![node_id]);
            self.file_scoped.insert(file_path.to_string(), m);
        }

        if let Some(list) = self.global_scoped.get_mut(node_name) {
            list.push(node_id);
        } else {
            self.global_scoped.insert(node_name.to_string(), vec![node_id]);
        }

        self.id_to_file.insert(node_id, file_path.to_string());

        self.node_kinds.push(kind);
        self.node_file_meta.push(file_meta);
    }

    /// Looks up a node ID by its file path and node name.
    ///
    /// Returns the **first** matching node_id (insertion order = source-line
    /// order via `parser.rs` Vec+idx pattern). Use [`lookup_in_file_with_kind`]
    /// when the caller knows the target's `ResolveTarget` — same-name nodes of
    /// other kinds would otherwise be the "winner" here and produce semantic-
    /// nonsense edges (e.g. `Calls -> Property`).
    pub fn lookup_in_file(&self, file_path: &str, node_name: &str) -> Option<u32> {
        self.file_scoped
            .get(file_path)
            .and_then(|file_map| file_map.get(node_name))
            .and_then(|ids| ids.first().copied())
    }

    /// Kind-aware variant of [`lookup_in_file`]: scans the per-name node_id
    /// list and returns the first whose `node_kinds[id]` matches the target
    /// predicate (Callable / Type). Skips same-name-different-kind nodes so
    /// resolver Tier-1 picks the semantically correct target — e.g. a call
    /// to `Foo()` in a file with both `class Foo` and `property Foo` lands
    /// on the constructor / method, never the property.
    pub fn lookup_in_file_with_kind(
        &self,
        file_path: &str,
        node_name: &str,
        target: ResolveTarget,
    ) -> Option<u32> {
        let ids = self.file_scoped.get(file_path)?.get(node_name)?;
        let predicate: fn(NodeKind) -> bool = match target {
            ResolveTarget::Callable => NodeKind::is_callable,
            ResolveTarget::Type => NodeKind::is_type,
        };
        ids.iter()
            .copied()
            .find(|&id| predicate(self.node_kinds[id as usize]))
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
        // `.h` is genuinely ambiguous between C and C++ headers; we route to
        // C++ because C++ parsing handles C as a near-subset, while C parsing
        // produces ERROR nodes on any C++ construct. See `from_normalized_path`.
        assert_eq!(Language::from_path("a/b.h"), Language::Cpp);
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
    fn dot_h_routes_to_cpp_not_c() {
        // Regression for ref-gitnexus parity: real codebases ship C++ headers
        // with `.h` extension (nlohmann/json, doctest, LLVM Fuzzer, Catch2,
        // most game engines). Routing them through the C parser silently
        // drops every class / template / namespace / method declaration.
        // Pure C compilation units stay with `.c`; only the ambiguous `.h`
        // moves to Cpp.
        assert_eq!(Language::from_path("foo.h"), Language::Cpp);
        assert_eq!(Language::from_path("path/to/header.h"), Language::Cpp);
        assert_eq!(Language::from_path("foo.c"), Language::C);
        // Backslash-normalised path takes the fast path; still routes correctly.
        assert_eq!(
            Language::from_normalized_path("Cpp/include/foo.h"),
            Language::Cpp
        );
        assert_eq!(
            Language::from_normalized_path("C/src/impl.c"),
            Language::C
        );
    }

    #[test]
    fn file_meta_detects_vendor_segment() {
        assert!(FileMeta::from_path("crates/vendor/tree-sitter-move/x.move").is_vendor);
        assert!(FileMeta::from_path("vendor/x.move").is_vendor);
        assert!(!FileMeta::from_path("crates/graph-nexus-analyzer/src/x.rs").is_vendor);
        assert!(!FileMeta::from_path("src/vendored_helper.rs").is_vendor);
    }
}
