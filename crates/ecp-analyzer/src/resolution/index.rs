use ecp_core::file_category::pick_global;
pub use ecp_core::file_category::{FileMeta, GlobalPick, Language};
use ecp_core::graph::NodeKind;
use rustc_hash::{FxHashMap, FxHashSet};

pub type NodeId = u32;

/// `node_owner` slot for a node with no owning type.
const NO_OWNER: u32 = u32::MAX;

/// Depth cap for [`SymbolTable::ancestors`]. Declared heritage deeper than
/// this is rare, and the cap bounds the walk on a corrupt or cyclic chain.
pub const MAX_HERITAGE_DEPTH: usize = 8;

/// Declared supertypes of one type node, resolved to type ids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Supertypes {
    /// Resolved bases in declared order, deduplicated, never the type itself.
    pub bases: Box<[u32]>,
    /// Number of resolved bases declared before the first unresolved one;
    /// `None` when every declared base resolved. Python's MRO consults an
    /// external mixin listed first before a later project base, so a member
    /// lookup needs the position, not only the flag.
    pub first_unresolved: Option<u32>,
}

impl Supertypes {
    pub fn has_unresolved_base(&self) -> bool {
        self.first_unresolved.is_some()
    }
}

/// The key under which an `owner_class` string is interned: the last
/// `.` / `::` / `\` segment with generic arguments cut, trimmed. Parsers
/// mostly emit a bare name (`Dog`), but C++ out-of-line definitions, Ruby
/// `class A::B` and generic impls can carry a path or `<T>`. Collapsing to
/// the last segment loses no identity: every owned lookup is also scoped to
/// one file or one crate root, and the caller treats several hits as
/// ambiguous. `None` for an empty owner (Rust inherent `impl` blocks).
fn owner_key(raw: &str) -> Option<&str> {
    let mut depth = 0u32;
    let mut seg_start = 0;
    let mut seg_end = None;
    for (i, c) in raw.char_indices() {
        match c {
            '<' | '[' => {
                if depth == 0 && seg_end.is_none() {
                    seg_end = Some(i);
                }
                depth += 1;
            }
            '>' | ']' => depth = depth.saturating_sub(1),
            '.' | ':' | '\\' if depth == 0 => {
                seg_start = i + c.len_utf8();
                seg_end = None;
            }
            _ => {}
        }
    }
    let key = raw[seg_start..seg_end.unwrap_or(raw.len())].trim();
    (!key.is_empty()).then_some(key)
}

/// The iterator's only item; `None` when it yields none or several.
fn single(mut items: impl Iterator<Item = u32>) -> Option<u32> {
    let first = items.next()?;
    items.next().is_none().then_some(first)
}

/// Crate-root prefix of a normalized repo-relative path. The "crate root"
/// here is the substring preceding the first `/src/` or `/tests/` segment,
/// which is enough to keep a workspace member's files together (every Rust
/// file in `crates/cli/src/...` shares prefix `crates/cli`) while keeping
/// external paths (the std library is never indexed in a workspace, so its
/// "prefix" never matches an indexed file's) outside the bucket.
///
/// Paths with no `/src/` or `/tests/` segment return `""` — single-crate
/// repos at the repo root all share the empty prefix, so the Tier-4
/// module-file fallback still fires for them.
#[cfg(not(windows))]
pub(crate) fn crate_root_prefix(path: &str) -> &str {
    path.rsplit_once("/src/")
        .or_else(|| path.rsplit_once("/tests/"))
        .map(|(root, _)| root)
        .unwrap_or("")
}

#[cfg(windows)]
pub(crate) fn crate_root_prefix(path: &str) -> &str {
    // Windows paths use backslashes natively.
    path.rsplit_once("\\src\\")
        .or_else(|| path.rsplit_once("\\tests\\"))
        .or_else(|| path.rsplit_once("/src/")) // Fallback for mixed/normalized paths
        .or_else(|| path.rsplit_once("/tests/"))
        .map(|(root, _)| root)
        .unwrap_or("")
}

/// Edge kinds the resolver resolves towards. Constrains Tier-3 fallback so a
/// bare callee like `format` never resolves to a Variable/Const that happens
/// to share the name.
///
/// `Qualifier` is the leading-segment lookup used by `resolve_qualifier_file`
/// — it accepts Type (Class/Struct/Enum/Typedef/Trait/Interface) plus
/// Namespace (C++ / C# / PHP) and Module (Rust inline `mod`). Without
/// these, every qualified call whose leading segment isn't a class /
/// struct / enum / typedef / trait / interface drops at Tier 2.5 since
/// the qualifier kind doesn't pass `is_type` — falling to the bare-name
/// Tier 3 which rejects ultra-common member names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveTarget {
    Callable,
    Type,
    Qualifier,
    /// A struct/class field, for `ReadsField` edge resolution. Filters to
    /// `NodeKind::is_property`.
    Field,
}

impl ResolveTarget {
    pub fn kind_predicate(self) -> fn(NodeKind) -> bool {
        match self {
            Self::Callable => NodeKind::is_callable,
            Self::Type => NodeKind::is_type,
            Self::Qualifier => NodeKind::is_qualifier,
            Self::Field => NodeKind::is_property,
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
    /// `lookup_global` to filter candidates without allocating side
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
    /// per failed-qualifier resolution (~3 k entries on the egent-code-plexus
    /// index, fires once per unresolved qualified call → millions of
    /// stem comparisons on cold-index build). The map collapses that to
    /// an O(1) lookup + O(candidates-per-stem) inner walk.
    stem_index: FxHashMap<String, Vec<String>>,

    /// Interned [`owner_key`] per node id (`NO_OWNER` when the node has no
    /// owning type), parallel to `node_kinds`. Answers "does type `T` own
    /// `member`" for the receiver-typing ladder: `file_scoped` alone returns
    /// the first same-named member in the file, whichever type owns it.
    node_owner: Vec<u32>,

    /// Owner key → interned id. One entry per distinct owner name, so the
    /// per-node cost is the 4-byte `node_owner` slot.
    owner_ids: FxHashMap<Box<str>, u32>,

    /// Declared supertypes per type id. Filled once by the builder's
    /// supertypes pre-pass, after Pass 1 and before Pass 2. Types with no
    /// declared heritage have no entry.
    supertypes: FxHashMap<u32, Supertypes>,

    /// Owner id → constructor node ids, so a constructed type finds its
    /// constructors whatever their name (`__init__`, `init`, `constructor`,
    /// the class name) and wherever they live (C++ out-of-line definitions,
    /// Swift extensions).
    constructors_by_owner: FxHashMap<u32, Vec<u32>>,
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
        self.stem_index.get(stem).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Registers a node with the given file path, node name, node ID, and kind.
    ///
    /// `node_id` must be the monotonic sequential index assigned by the builder
    /// (debug-asserted), so `node_kinds[id]` / `node_file_meta[id]` indexing
    /// works in `lookup_global`.
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
            let has_primary = entry
                .iter()
                .any(|&id| !matches!(self.node_kinds.get(id as usize), Some(NodeKind::Impl)));
            if !has_primary {
                entry.push(node_id);
            }
        } else {
            // Non-Impl: remove any prior Impl-only placeholders, then push.
            entry.retain(|&id| !matches!(self.node_kinds.get(id as usize), Some(NodeKind::Impl)));
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
        self.node_owner.push(NO_OWNER);
    }

    /// [`register_node`] plus an owner, for tests that exercise the owner
    /// index without running the builder.
    pub fn register_node_owned(
        &mut self,
        file_path: &str,
        node_name: &str,
        node_id: u32,
        kind: NodeKind,
        owner: &str,
    ) {
        self.register_node(file_path, node_name, node_id, kind);
        let owner_id = self.intern_owner(Some(owner));
        self.node_owner[node_id as usize] = owner_id;
    }

    fn intern_owner(&mut self, owner: Option<&str>) -> u32 {
        let Some(key) = owner.and_then(owner_key) else {
            return NO_OWNER;
        };
        if let Some(&id) = self.owner_ids.get(key) {
            return id;
        }
        let id = self.owner_ids.len() as u32;
        self.owner_ids.insert(key.into(), id);
        id
    }

    fn owner_id(&self, owner: &str) -> Option<u32> {
        self.owner_ids.get(owner_key(owner)?).copied()
    }

    /// Hot-path variant of `register_node` for callers that already
    /// computed `FileMeta` for this file (i.e. Pass 1 hoists `FileMeta`
    /// once per file out of the per-node loop, since 1 file ↔ ~25 nodes
    /// on `.sample_repo` and `FileMeta::from_path` allocates one `String`
    /// per call for the `\\` → `/` replace). Semantically identical to
    /// `register_node` but skips the redundant per-node path parse, and
    /// records `owner` (the node's `owner_class`) in the owner index.
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
        owner: Option<&str>,
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
            self.global_scoped
                .insert(node_name.to_string(), vec![node_id]);
        }

        self.id_to_file.insert(node_id, file_path.to_string());

        self.node_kinds.push(kind);
        self.node_file_meta.push(file_meta);
        let owner_id = self.intern_owner(owner);
        self.node_owner.push(owner_id);
        if kind == NodeKind::Constructor && owner_id != NO_OWNER {
            self.constructors_by_owner
                .entry(owner_id)
                .or_default()
                .push(node_id);
        }
    }

    /// Register a tombstone node: advances `node_kinds` / `node_file_meta`
    /// alignment (keeping the monotonic-dense invariant intact) WITHOUT adding
    /// the node to `file_scoped`, `global_scoped`, or `id_to_file`. Tombstones
    /// occupy a node-ID slot so that subsequent registrations get the correct
    /// ID, but they are invisible to all name-based lookups — no edge emitter
    /// can obtain a tombstone node_id via `lookup_in_file` / `lookup_global`.
    ///
    /// Used for UID-collision-dropped nodes (D1 recovery): the colliding raw node
    /// still needs to occupy a position in `nodes` (to keep `start_indices`
    /// prefix-sums correct in Pass 2), but must not be reachable by name.
    pub fn register_tombstone(&mut self, kind: NodeKind, file_meta: FileMeta) {
        self.node_kinds.push(kind);
        self.node_file_meta.push(file_meta);
        self.node_owner.push(NO_OWNER);
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
        let predicate = target.kind_predicate();
        ids.iter()
            .copied()
            .find(|&id| predicate(self.node_kinds[id as usize]))
    }

    /// Tier-3 global lookup: kind-filtered same-name candidates through the
    /// shared barrier filter, [`pick_global`].
    pub fn lookup_global(
        &self,
        node_name: &str,
        target: ResolveTarget,
        caller: FileMeta,
    ) -> GlobalPick {
        let Some(raw) = self.global_scoped.get(node_name) else {
            return GlobalPick::NoMatch;
        };
        let predicate = target.kind_predicate();
        pick_global(
            caller,
            raw.iter()
                .filter(|&&id| predicate(self.node_kinds[id as usize]))
                .map(|&id| (id, self.node_file_meta[id as usize])),
        )
    }

    /// Every candidate [`lookup_global`] weighs, as a set: kind-filtered, then
    /// through the same [`FileMeta::admits`] barrier [`pick_global`] applies.
    /// A test double that shares a production name survives here exactly as
    /// it keeps `pick_global` ambiguous; the caller decides among the set.
    pub fn global_candidates(
        &self,
        node_name: &str,
        target: ResolveTarget,
        caller: FileMeta,
    ) -> Vec<u32> {
        let Some(raw) = self.global_scoped.get(node_name) else {
            return Vec::new();
        };
        let predicate = target.kind_predicate();
        raw.iter()
            .copied()
            .filter(|&id| {
                predicate(self.node_kinds[id as usize])
                    && caller.admits(self.node_file_meta[id as usize])
            })
            .collect()
    }

    /// Members named `member` in `file_path` whose owner is `owner`, in
    /// source order, kind-filtered by `target`. Several hits mean the file
    /// holds two same-named owners (nested types, or a type and a module
    /// sharing a name); the caller treats that as ambiguous.
    pub fn owned_in_file(
        &self,
        file_path: &str,
        member: &str,
        owner: &str,
        target: ResolveTarget,
    ) -> Vec<u32> {
        let (Some(owner_id), Some(ids)) = (
            self.owner_id(owner),
            self.file_scoped.get(file_path).and_then(|m| m.get(member)),
        ) else {
            return Vec::new();
        };
        let predicate = target.kind_predicate();
        ids.iter()
            .copied()
            .filter(|&id| {
                self.node_owner[id as usize] == owner_id && predicate(self.node_kinds[id as usize])
            })
            .collect()
    }

    /// Members named `member` owned by `owner` anywhere in the scope of
    /// `scope_file` (the file that declares the owning type): the same
    /// language, vendor barrier and crate root, and for Go the same
    /// directory (one package). Covers members declared outside the type's
    /// file: Rust `impl` blocks, Go methods, C# partials, Swift extensions,
    /// Ruby class reopening.
    pub fn owned_global(
        &self,
        member: &str,
        owner: &str,
        target: ResolveTarget,
        scope_file: &str,
    ) -> Vec<u32> {
        let (Some(owner_id), Some(ids)) = (self.owner_id(owner), self.global_scoped.get(member))
        else {
            return Vec::new();
        };
        let scope_meta = FileMeta::from_path(scope_file);
        let scope_root = crate_root_prefix(scope_file);
        let same_package_only = scope_meta.language == Language::Go;
        let scope_dir = std::path::Path::new(scope_file).parent();
        let predicate = target.kind_predicate();
        ids.iter()
            .copied()
            .filter(|&id| {
                let i = id as usize;
                self.node_owner[i] == owner_id
                    && predicate(self.node_kinds[i])
                    && scope_meta.admits(self.node_file_meta[i])
                    && self.file_of(id).is_some_and(|f| {
                        crate_root_prefix(f) == scope_root
                            && (!same_package_only || std::path::Path::new(f).parent() == scope_dir)
                    })
            })
            .collect()
    }

    /// True when some Class / Struct is named `name`. The constructor
    /// fallback runs on every unresolved call, so this is its first gate.
    pub fn has_constructible_type(&self, name: &str) -> bool {
        self.global_scoped.get(name).is_some_and(|ids| {
            ids.iter()
                .any(|&id| self.node_kinds[id as usize].is_constructible())
        })
    }

    fn declares_constructible_type(&self, file_path: &str, name: &str) -> bool {
        self.file_scoped
            .get(file_path)
            .and_then(|m| m.get(name))
            .is_some_and(|ids| {
                ids.iter()
                    .any(|&id| self.node_kinds[id as usize].is_constructible())
            })
    }

    /// The one constructor a construction of type `type_id` (named
    /// `type_name`) calls. `None` when the type declares no constructor or
    /// several. Overloads are not several: Pass 1 collapses them into one
    /// node per (kind, path, owner, name) uid, which then stands for "a
    /// constructor of the type". The type's own file decides when it
    /// declares any constructor; otherwise the type's scope counts (C++
    /// out-of-line definitions, Swift extensions), except a file that
    /// declares another type of that name.
    pub fn sole_constructor(&self, type_id: u32, type_name: &str) -> Option<u32> {
        let ctors = self.constructors_by_owner.get(&self.owner_id(type_name)?)?;
        let type_file = self.file_of(type_id)?;
        let in_type_file = |id: &u32| self.file_of(*id) == Some(type_file);
        if ctors.iter().any(in_type_file) {
            single(ctors.iter().copied().filter(|id| in_type_file(id)))
        } else {
            let scope_meta = self.node_file_meta[type_id as usize];
            let scope_root = crate_root_prefix(type_file);
            single(ctors.iter().copied().filter(|&id| {
                scope_meta.admits(self.node_file_meta[id as usize])
                    && self.file_of(id).is_some_and(|f| {
                        crate_root_prefix(f) == scope_root
                            && !self.declares_constructible_type(f, type_name)
                    })
            }))
        }
    }

    /// Number of nodes named `node_name` in `file_path` that match `target`.
    pub fn count_in_file_with_kind(
        &self,
        file_path: &str,
        node_name: &str,
        target: ResolveTarget,
    ) -> usize {
        let Some(ids) = self
            .file_scoped
            .get(file_path)
            .and_then(|m| m.get(node_name))
        else {
            return 0;
        };
        let predicate = target.kind_predicate();
        ids.iter()
            .filter(|&&id| predicate(self.node_kinds[id as usize]))
            .count()
    }

    /// True when at least one node is registered under `file_path`, i.e. the
    /// file belongs to the indexed project.
    pub fn has_file(&self, file_path: &str) -> bool {
        self.file_scoped.contains_key(file_path)
    }

    /// The name `node_id` is registered under in its file. The index keeps no
    /// id → name map; this scans the names of one file. The receiver-typing
    /// ladder calls it only for the declared supertypes it walks.
    pub fn name_in_file(&self, node_id: u32) -> Option<&str> {
        let file = self.id_to_file.get(&node_id)?;
        self.file_scoped
            .get(file)?
            .iter()
            .find(|(_, ids)| ids.contains(&node_id))
            .map(|(name, _)| name.as_str())
    }

    /// Replace the supertypes map from `(type_id, base)` pairs in declared
    /// order; `None` marks a base that did not resolve to a project type.
    /// Repeated bases collapse to their first position, and a type never
    /// lists itself (a base name that resolves back to the declaring type).
    pub fn build_supertypes(&mut self, declared: impl IntoIterator<Item = (u32, Option<u32>)>) {
        let mut acc: FxHashMap<u32, (Vec<u32>, Option<u32>)> = FxHashMap::default();
        for (ty, base) in declared {
            let (bases, first_unresolved) = acc.entry(ty).or_default();
            match base {
                None if first_unresolved.is_none() => {
                    *first_unresolved = Some(bases.len() as u32);
                }
                None => {}
                Some(b) if b != ty && !bases.contains(&b) => bases.push(b),
                Some(_) => {}
            }
        }
        self.supertypes = acc
            .into_iter()
            .map(|(ty, (bases, first_unresolved))| {
                (
                    ty,
                    Supertypes {
                        bases: bases.into_boxed_slice(),
                        first_unresolved,
                    },
                )
            })
            .collect();
    }

    /// Declared supertypes of `type_id`; `None` when it declares no heritage.
    pub fn supertypes(&self, type_id: u32) -> Option<&Supertypes> {
        self.supertypes.get(&type_id)
    }

    /// Transitive resolved supertypes of `type_id` in breadth-first order,
    /// each declared level in declared order. Every type appears once, a
    /// heritage cycle ends at the first revisit, and the walk stops after
    /// [`MAX_HERITAGE_DEPTH`] levels. `type_id` itself is excluded.
    pub fn ancestors(&self, type_id: u32) -> Vec<u32> {
        let mut seen: FxHashSet<u32> = FxHashSet::default();
        seen.insert(type_id);
        let mut out = Vec::new();
        let mut frontier = vec![type_id];
        for _ in 0..MAX_HERITAGE_DEPTH {
            let level_start = out.len();
            for ty in frontier {
                let Some(st) = self.supertypes.get(&ty) else {
                    continue;
                };
                for &base in st.bases.iter() {
                    if seen.insert(base) {
                        out.push(base);
                    }
                }
            }
            if out.len() == level_start {
                break;
            }
            frontier = out[level_start..].to_vec();
        }
        out
    }

    /// Total count of same-named candidates (before kind/locality filters).
    /// Exposed for the resolver decision dump's `alt_count` telemetry.
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

    /// O(1) kind lookup by node id. Used by Pass-2 heritage dispatch to
    /// distinguish Interface/Trait targets (→ Implements) from concrete class
    /// targets (→ Extends) without a secondary resolver round-trip.
    ///
    /// Returns `NodeKind::File` (the `#[default]` variant) for ids outside
    /// the registered range — tombstone slots still push a kind, so valid ids
    /// are always in-range; an out-of-range id signals a resolver bug and the
    /// safe fallback is the non-interface default.
    pub fn node_kind(&self, node_id: u32) -> NodeKind {
        self.node_kinds
            .get(node_id as usize)
            .copied()
            .unwrap_or_default()
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
        assert_eq!(Language::from_normalized_path("C/src/impl.c"), Language::C);
    }

    #[test]
    fn file_meta_detects_vendor_segment() {
        assert!(FileMeta::from_path("crates/vendor/tree-sitter-move/x.move").is_vendor);
        assert!(FileMeta::from_path("vendor/x.move").is_vendor);
        assert!(!FileMeta::from_path("crates/ecp-analyzer/src/x.rs").is_vendor);
        assert!(!FileMeta::from_path("src/vendored_helper.rs").is_vendor);
    }

    // ---- owner index ----

    /// `(file, name, kind, owner)` rows; ids are the row positions.
    fn st_owned(rows: &[(&str, &str, NodeKind, Option<&str>)]) -> SymbolTable {
        let mut st = SymbolTable::new();
        for (id, &(file, name, kind, owner)) in rows.iter().enumerate() {
            match owner {
                Some(o) => st.register_node_owned(file, name, id as u32, kind, o),
                None => st.register_node(file, name, id as u32, kind),
            }
        }
        st
    }

    const CALL: ResolveTarget = ResolveTarget::Callable;

    #[test]
    fn test_owned_in_file_method_of_same_file_type_returns_member() {
        let st = st_owned(&[
            ("a.py", "Tx", NodeKind::Class, None),
            ("a.py", "execute", NodeKind::Method, Some("Tx")),
        ]);
        assert_eq!(st.owned_in_file("a.py", "execute", "Tx", CALL), vec![1]);
        assert!(
            st.owned_in_file("b.py", "execute", "Tx", CALL).is_empty(),
            "the lookup is scoped to the given file"
        );
        assert!(
            st.owned_in_file("a.py", "execute", "Tx", ResolveTarget::Type)
                .is_empty(),
            "the kind filter still applies"
        );
    }

    #[test]
    fn test_owned_in_file_same_named_methods_on_two_types_returns_each_owners_member() {
        let st = st_owned(&[
            ("db.py", "Tx", NodeKind::Class, None),
            ("db.py", "execute", NodeKind::Method, Some("Tx")),
            ("db.py", "BaseDBPool", NodeKind::Class, None),
            ("db.py", "execute", NodeKind::Method, Some("BaseDBPool")),
        ]);
        assert_eq!(
            st.lookup_in_file_with_kind("db.py", "execute", CALL),
            Some(1),
            "the name-only lookup returns the first member in source order (F2)"
        );
        assert_eq!(st.owned_in_file("db.py", "execute", "Tx", CALL), vec![1]);
        assert_eq!(
            st.owned_in_file("db.py", "execute", "BaseDBPool", CALL),
            vec![3]
        );
    }

    #[test]
    fn test_owned_in_file_free_function_has_no_owner_returns_empty() {
        let st = st_owned(&[
            ("util.py", "helper", NodeKind::Function, None),
            ("util.py", "util", NodeKind::Class, None),
        ]);
        assert!(st
            .owned_in_file("util.py", "helper", "util", CALL)
            .is_empty());
        assert!(st.owned_in_file("util.py", "helper", "", CALL).is_empty());
    }

    #[test]
    fn test_owned_in_file_empty_owner_string_registers_no_owner_returns_empty() {
        // Rust inherent `impl` blocks carry owner `""`.
        let st = st_owned(&[("src/lib.rs", "run", NodeKind::Method, Some(""))]);
        assert!(st.owned_in_file("src/lib.rs", "run", "", CALL).is_empty());
        assert!(st.owner_ids.is_empty(), "an empty owner is not interned");
    }

    #[test]
    fn test_owned_in_file_generic_owner_matches_bare_type_name_returns_member() {
        let st = st_owned(&[
            ("box.hpp", "Box", NodeKind::Class, None),
            ("box.hpp", "get", NodeKind::Method, Some("Box<T>")),
        ]);
        assert_eq!(st.owned_in_file("box.hpp", "get", "Box", CALL), vec![1]);
        assert_eq!(st.owned_in_file("box.hpp", "get", "Box<U>", CALL), vec![1]);
    }

    #[test]
    fn test_owned_in_file_nested_type_owner_keys_on_last_segment_returns_inner_member() {
        let st = st_owned(&[
            ("Outer.java", "Outer", NodeKind::Class, None),
            ("Outer.java", "run", NodeKind::Method, Some("Outer")),
            ("Outer.java", "Inner", NodeKind::Class, Some("Outer")),
            ("Outer.java", "run", NodeKind::Method, Some("Outer.Inner")),
        ]);
        assert_eq!(
            st.owned_in_file("Outer.java", "run", "Inner", CALL),
            vec![3]
        );
        assert_eq!(
            st.owned_in_file("Outer.java", "run", "Outer", CALL),
            vec![1]
        );
    }

    #[test]
    fn test_owned_in_file_type_and_module_share_name_returns_both_members() {
        // The owner index keys on the name only; two same-named owners in one
        // file both answer, and the caller must treat that as ambiguous.
        let st = st_owned(&[
            ("cfg.rb", "Config", NodeKind::Class, None),
            ("cfg.rb", "load", NodeKind::Method, Some("Config")),
            ("cfg.rb", "Config", NodeKind::Module, None),
            ("cfg.rb", "load", NodeKind::Function, Some("Config")),
        ]);
        assert_eq!(
            st.owned_in_file("cfg.rb", "load", "Config", CALL),
            vec![1, 3]
        );
    }

    #[test]
    fn test_name_in_file_registered_and_unknown_ids_returns_name_or_none() {
        let st = st_owned(&[
            ("a.py", "Base", NodeKind::Class, None),
            ("a.py", "greet", NodeKind::Method, Some("Base")),
        ]);
        assert_eq!(st.name_in_file(0), Some("Base"));
        assert_eq!(st.name_in_file(1), Some("greet"));
        assert_eq!(st.name_in_file(7), None);
    }

    #[test]
    fn test_count_in_file_with_kind_type_and_module_share_name_counts_by_kind() {
        let st = st_owned(&[
            ("cfg.rb", "Config", NodeKind::Class, None),
            ("cfg.rb", "Config", NodeKind::Module, None),
        ]);
        assert_eq!(
            st.count_in_file_with_kind("cfg.rb", "Config", ResolveTarget::Qualifier),
            2
        );
        assert_eq!(
            st.count_in_file_with_kind("cfg.rb", "Config", ResolveTarget::Type),
            1
        );
        assert_eq!(
            st.count_in_file_with_kind("other.rb", "Config", ResolveTarget::Type),
            0
        );
    }

    #[test]
    fn test_has_file_indexed_and_unknown_paths_returns_presence() {
        let st = st_owned(&[("pkg/models.py", "Repo", NodeKind::Class, None)]);
        assert!(st.has_file("pkg/models.py"));
        assert!(!st.has_file("psqlpy"));
    }

    #[test]
    fn test_owner_key_paths_and_generics_returns_last_bare_segment() {
        for (raw, want) in [
            ("Dog", Some("Dog")),
            ("Foo<T>", Some("Foo")),
            ("ns::Foo", Some("Foo")),
            ("A::B", Some("B")),
            ("Outer.Inner", Some("Inner")),
            ("\\App\\Model", Some("Model")),
            ("Map<K, V>::Entry", Some("Entry")),
            ("Foo<a::B>", Some("Foo")),
            ("List[int]", Some("List")),
            ("  Dog ", Some("Dog")),
            ("", None),
            ("   ", None),
            ("Foo::", None),
        ] {
            assert_eq!(owner_key(raw), want, "owner_key({raw:?})");
        }
    }

    #[test]
    fn test_register_tombstone_keeps_owner_slots_aligned_returns_later_member() {
        let meta = FileMeta::from_path("a.py");
        let mut st = SymbolTable::new();
        st.register_node_with_meta("a.py", meta, "A", 0, NodeKind::Class, None);
        st.register_tombstone(NodeKind::Method, meta);
        st.register_node_with_meta("a.py", meta, "m", 2, NodeKind::Method, Some("A"));
        assert_eq!(st.node_owner, vec![NO_OWNER, NO_OWNER, 0]);
        assert_eq!(st.owned_in_file("a.py", "m", "A", CALL), vec![2]);
    }

    #[test]
    fn test_owned_global_rust_impl_in_sibling_file_same_crate_returns_member() {
        let st = st_owned(&[
            ("crates/x/src/model.rs", "Foo", NodeKind::Struct, None),
            (
                "crates/x/src/model/ops.rs",
                "bar",
                NodeKind::Method,
                Some("Foo"),
            ),
            (
                "crates/y/src/model.rs",
                "bar",
                NodeKind::Method,
                Some("Foo"),
            ),
            ("crates/x/src/gen.py", "bar", NodeKind::Method, Some("Foo")),
            (
                "crates/x/src/other.rs",
                "bar",
                NodeKind::Method,
                Some("Other"),
            ),
        ]);
        assert!(
            st.owned_in_file("crates/x/src/model.rs", "bar", "Foo", CALL)
                .is_empty(),
            "the impl lives in a sibling file"
        );
        assert_eq!(
            st.owned_global("bar", "Foo", CALL, "crates/x/src/model.rs"),
            vec![1],
            "another crate, another language and another owner are all excluded"
        );
    }

    #[test]
    fn test_owned_global_go_method_in_other_package_excluded_returns_same_package_only() {
        let st = st_owned(&[
            ("pkg/a/types.go", "Store", NodeKind::Struct, None),
            ("pkg/a/methods.go", "Get", NodeKind::Method, Some("Store")),
            ("pkg/b/methods.go", "Get", NodeKind::Method, Some("Store")),
        ]);
        assert_eq!(
            st.owned_global("Get", "Store", CALL, "pkg/a/types.go"),
            vec![1]
        );
    }

    #[test]
    fn test_owned_global_csharp_partial_in_sibling_file_returns_member() {
        let st = st_owned(&[
            ("src/Foo.cs", "Foo", NodeKind::Class, None),
            ("src/Foo.Generated.cs", "Bar", NodeKind::Method, Some("Foo")),
            (
                "vendor/lib/src/Foo.cs",
                "Bar",
                NodeKind::Method,
                Some("Foo"),
            ),
        ]);
        assert_eq!(st.owned_global("Bar", "Foo", CALL, "src/Foo.cs"), vec![1]);
    }

    #[test]
    fn test_owned_global_unknown_owner_or_member_returns_empty() {
        let st = st_owned(&[("src/a.rs", "bar", NodeKind::Method, Some("Foo"))]);
        assert!(st.owned_global("bar", "Nope", CALL, "src/a.rs").is_empty());
        assert!(st.owned_global("nope", "Foo", CALL, "src/a.rs").is_empty());
        assert!(st.owned_global("bar", "", CALL, "src/a.rs").is_empty());
    }

    // ---- global_candidates ----

    #[test]
    fn test_global_candidates_test_double_kept_like_pick_global_returns_both() {
        let st = st_owned(&[
            ("src/svc.py", "Svc", NodeKind::Class, None),
            ("tests/test_svc.py", "Svc", NodeKind::Class, None),
        ]);
        let caller = FileMeta::from_path("src/app.py");
        assert_eq!(
            st.lookup_global("Svc", ResolveTarget::Type, caller),
            GlobalPick::Ambiguous,
            "pick_global keeps a test double ambiguous"
        );
        assert_eq!(
            st.global_candidates("Svc", ResolveTarget::Type, caller),
            vec![0, 1],
            "the candidate set applies the same filter"
        );
    }

    #[test]
    fn test_global_candidates_cross_language_vendor_and_kind_excluded_returns_survivors() {
        let st = st_owned(&[
            ("a/svc.py", "Svc", NodeKind::Class, None),
            ("web/svc.ts", "Svc", NodeKind::Class, None),
            ("vendor/x/svc.py", "Svc", NodeKind::Class, None),
            ("b/svc.py", "Svc", NodeKind::Class, None),
            ("c/svc.py", "Svc", NodeKind::Function, None),
        ]);
        let caller = FileMeta::from_path("app.py");
        assert_eq!(
            st.global_candidates("Svc", ResolveTarget::Type, caller),
            vec![0, 3]
        );
        assert_eq!(
            st.global_candidates(
                "Svc",
                ResolveTarget::Type,
                FileMeta::from_path("vendor/y/m.py")
            ),
            vec![0, 2, 3],
            "a vendor caller may reach vendor candidates, as in pick_global"
        );
    }

    #[test]
    fn test_global_candidates_unique_or_unknown_name_matches_pick_global() {
        let st = st_owned(&[("a.py", "Only", NodeKind::Class, None)]);
        let caller = FileMeta::from_path("b.py");
        assert_eq!(
            st.lookup_global("Only", ResolveTarget::Type, caller),
            GlobalPick::Unique(0)
        );
        assert_eq!(
            st.global_candidates("Only", ResolveTarget::Type, caller),
            vec![0]
        );
        assert!(st
            .global_candidates("Missing", ResolveTarget::Type, caller)
            .is_empty());
    }

    // ---- supertypes ----

    fn st_with_heritage(declared: &[(u32, Option<u32>)]) -> SymbolTable {
        let mut st = SymbolTable::new();
        st.build_supertypes(declared.iter().copied());
        st
    }

    #[test]
    fn test_build_supertypes_declared_order_kept_returns_bases_in_order() {
        let st = st_with_heritage(&[(0, Some(2)), (0, Some(1)), (0, Some(2))]);
        let s = st.supertypes(0).expect("type 0 declares heritage");
        assert_eq!(&*s.bases, &[2, 1], "declared order, repeats collapsed");
        assert!(!s.has_unresolved_base());
        assert!(st.supertypes(1).is_none(), "no heritage, no entry");
    }

    #[test]
    fn test_build_supertypes_unresolved_base_sets_flag_and_position() {
        let st = st_with_heritage(&[
            (0, Some(1)),
            (0, None),
            (0, Some(2)),
            (0, None),
            (5, None),
            (5, Some(1)),
        ]);
        let s = st.supertypes(0).unwrap();
        assert_eq!(&*s.bases, &[1, 2]);
        assert_eq!(
            s.first_unresolved,
            Some(1),
            "the first unresolved base sits after one resolved base"
        );
        let first = st.supertypes(5).unwrap();
        assert!(first.has_unresolved_base());
        assert_eq!(
            first.first_unresolved,
            Some(0),
            "an external base listed first precedes every project base"
        );
    }

    #[test]
    fn test_ancestors_heritage_cycle_terminates_returns_each_type_once() {
        let st = st_with_heritage(&[(0, Some(1)), (1, Some(2)), (2, Some(0)), (3, Some(3))]);
        assert_eq!(st.ancestors(0), vec![1, 2]);
        assert_eq!(st.ancestors(2), vec![0, 1]);
        assert!(
            st.supertypes(3).unwrap().bases.is_empty(),
            "a base resolving to the declaring type is dropped"
        );
        assert!(st.ancestors(3).is_empty());
    }

    #[test]
    fn test_ancestors_diamond_visits_shared_base_once_breadth_first() {
        // D(3) -> B(1), C(2); B -> A(0); C -> A(0).
        let st = st_with_heritage(&[(3, Some(1)), (3, Some(2)), (1, Some(0)), (2, Some(0))]);
        assert_eq!(st.ancestors(3), vec![1, 2, 0]);
    }

    #[test]
    fn test_ancestors_long_chain_stops_at_depth_cap() {
        let chain: Vec<(u32, Option<u32>)> = (0..20).map(|i| (i, Some(i + 1))).collect();
        let st = st_with_heritage(&chain);
        assert_eq!(
            st.ancestors(0),
            (1..=MAX_HERITAGE_DEPTH as u32).collect::<Vec<_>>()
        );
        assert!(st.ancestors(20).is_empty(), "a root type has no ancestors");
    }
}
