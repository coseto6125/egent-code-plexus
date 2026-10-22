use ecp_core::analyzer::import_binding::{
    fqn_language, owner_path_agrees, DeclaredType, MemberOwner,
};
use ecp_core::analyzer::reexport::{is_es_module, pick_call, ExportAliasTable};
use ecp_core::analyzer::same_file_pick::{self, ImplMember as PickImplMember, PickFacts};
use ecp_core::analyzer::type_ladder::{return_type_key, TypeFacts};
use ecp_core::analyzer::types::{owner_key, RawExportAlias};
use ecp_core::file_category::pick_global;
pub use ecp_core::file_category::{FileMeta, GlobalPick, Language};
use ecp_core::graph::NodeKind;
use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;

pub type NodeId = u32;

/// `node_owner` slot for a node with no owning type.
const NO_OWNER: u32 = u32::MAX;

/// Depth cap for [`SymbolTable::ancestors`], shared with the
/// receiver-typing ladder.
pub use ecp_core::analyzer::type_ladder::MAX_HERITAGE_DEPTH;

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

/// A Rust callable declared in an `impl` block ([`SymbolTable::note_impl_member`]).
#[derive(Debug, Clone, Copy)]
struct ImplMember {
    /// Identifies the block: no two members of one block share a name.
    block: u32,
    /// Owner id of the trait the block implements; `NO_OWNER` for an
    /// inherent `impl`.
    trait_owner: u32,
}

pub(crate) use ecp_core::analyzer::same_file_pick::PickScope;

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

pub use ecp_core::analyzer::reexport::ResolveTarget;

/// The kinds a Tier-3 lookup weighs for `target`. Method syntax narrows the
/// result, not the field: a same-named free fn still makes the name
/// ambiguous, so `path.join(..)` on a std type never binds the project's
/// only `join` method. A bare `f()` that never reaches a member
/// (`free_only`) drops every `Method` from the field instead: the method of
/// an unrelated class neither binds the call nor competes with the free
/// function the call does reach.
fn global_field(target: ResolveTarget, free_only: bool) -> impl Fn(NodeKind) -> bool {
    let field = match target {
        ResolveTarget::Method => ResolveTarget::Callable,
        other => other,
    }
    .kind_predicate();
    move |kind| field(kind) && !(free_only && kind == NodeKind::Method)
}

/// The last two components of `path`: its directory's name and its file
/// name (`collect/Maps.java`), or the whole path at the root.
fn leaf_path(path: &str) -> &str {
    let Some((dir, _)) = path.rsplit_once('/') else {
        return path;
    };
    dir.rfind('/').map_or(path, |slash| &path[slash + 1..])
}

/// The directories of `path`, root first.
fn path_dirs(path: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    parts
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
    /// Empty package entries participate in namespace discovery, not the
    /// existing symbol-backed module-stem fallback.
    empty_stem_index: FxHashMap<String, Vec<String>>,
    /// `.`-joined namespace name (PHP `A\B` is keyed `A.B`) → files of a
    /// fully qualified language declaring it, filled as Pass 1 registers
    /// Namespace nodes. FQN import discovery reads it: a Kotlin `package` or
    /// PHP `namespace` need not match the file's directory.
    namespace_files: FxHashMap<String, Vec<String>>,
    /// The inverse of `namespace_files`: file → the namespaces it declares.
    file_namespaces: FxHashMap<String, Vec<String>>,
    indexed_module_names:
        FxHashMap<std::mem::Discriminant<Language>, rustc_hash::FxHashSet<String>>,

    /// Interned [`owner_key`] per node id (`NO_OWNER` when the node has no
    /// owning type), parallel to `node_kinds`. Answers "does type `T` own
    /// `member`" for the receiver-typing ladder: `file_scoped` alone returns
    /// the first same-named member in the file, whichever type owns it.
    node_owner: Vec<u32>,

    /// Owner key → interned id. One entry per distinct owner name, so the
    /// per-node cost is the 4-byte `node_owner` slot.
    owner_ids: FxHashMap<Box<str>, u32>,

    /// The inverse of `owner_ids`: owner id → owner key.
    owner_keys: Vec<Box<str>>,

    /// First declaration → its later overloads, in source order (see
    /// [`SymbolTable::register_overload`]). Sparse: most ids have none.
    overloads: FxHashMap<u32, Vec<u32>>,

    /// Declared supertypes per type id. Filled once by the builder's
    /// supertypes pre-pass, after Pass 1 and before Pass 2. Types with no
    /// declared heritage have no entry.
    supertypes: FxHashMap<u32, Supertypes>,

    /// The `impl` block and implemented trait of each Rust member, see
    /// [`Self::note_impl_member`]. Sparse: only Rust callables declared in
    /// an `impl` block have an entry.
    impl_members: FxHashMap<u32, ImplMember>,

    /// `(file, owner key)` of every Namespace / Module declaration, the
    /// input of [`Self::build_owner_kinds`].
    namespace_decls: Vec<(Box<str>, Box<str>)>,

    /// File → the owner ids that name a namespace or module the file
    /// declares, and no type the file declares. Filled once after Pass 1 by
    /// [`Self::build_owner_kinds`]; the same-file pick rules
    /// ([`Self::prefer_owned`]) read such an owner as no owner.
    namespace_owners: FxHashMap<Box<str>, Vec<u32>>,

    /// Every Constructor registered with an owner, in id order: the input of
    /// [`SymbolTable::build_constructor_index`].
    constructors: Vec<u32>,

    /// Class / Struct id → its one constructor, whatever that is named
    /// (`__init__`, `init`, `constructor`, the class name). Filled once
    /// after Pass 1 by [`SymbolTable::build_constructor_index`].
    sole_constructors: FxHashMap<u32, u32>,

    /// Names some Class / Struct is registered under: the first gate of the
    /// constructor fallback, which runs on every unresolved call.
    constructible_names: FxHashSet<Box<str>>,

    /// Names some `EnumVariant` is registered under: the first gate of
    /// every enum-variant reference, most of which name no variant.
    enum_variant_names: FxHashSet<Box<str>>,

    /// Exported declarations of the files in `es_modules`, see
    /// [`ecp_core::analyzer::reexport::lookup_export_in_file`]. Sparse: only
    /// TS / JS nodes the parser marks `is_exported` are listed.
    exported_ids: FxHashSet<u32>,

    /// TS / JS files that export at least one declaration, see
    /// [`SymbolTable::note_export`].
    es_modules: FxHashSet<Box<str>>,

    /// The export names of the files in `es_modules` that bind a declaration
    /// of another name, see [`SymbolTable::note_export_aliases`]. Sparse:
    /// only files with such an export have an entry.
    export_aliases: FxHashMap<Box<str>, ExportAliasTable>,

    /// Function id → its interned [`return_type_key`]: the receiver type of
    /// a chained call on it (`make().bar()`). Sparse: only functions whose
    /// declared return type reduces to a bare name, filled by
    /// [`Self::note_return_type`] after Pass 1.
    return_types: FxHashMap<u32, u32>,

    /// Return-type key → interned id, one entry per distinct type name.
    return_type_ids: FxHashMap<Box<str>, u32>,

    /// The inverse of `return_type_ids`.
    return_type_keys: Vec<Box<str>>,
}

impl SymbolTable {
    /// Creates a new empty `SymbolTable`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Import-only package entry files still establish an indexed module.
    pub(crate) fn register_file(&mut self, path: &str) {
        self.file_scoped.entry(path.to_string()).or_default();
    }

    /// Populate the `stem_index` from the file paths already in
    /// `file_scoped`. Call exactly once after Pass 1 finishes registering
    /// nodes, before any resolver tier reads from the index. Idempotent
    /// (clears before rebuild) so a future caller adding files post-Pass-1
    /// can re-finalize without leaking stale entries.
    pub fn build_stem_index(&mut self) {
        self.stem_index.clear();
        self.empty_stem_index.clear();
        self.indexed_module_names.clear();
        for (path, symbols) in &self.file_scoped {
            let Some(stem) = std::path::Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
            else {
                continue;
            };
            let names = self
                .indexed_module_names
                .entry(std::mem::discriminant(&Language::from_normalized_path(
                    path,
                )))
                .or_default();
            names.insert(stem.to_string());
            if let Some(parent) = std::path::Path::new(path).parent() {
                names.extend(
                    parent
                        .components()
                        .filter_map(|component| component.as_os_str().to_str().map(str::to_string)),
                );
            }
            let index = if symbols.is_empty() {
                &mut self.empty_stem_index
            } else {
                &mut self.stem_index
            };
            index
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

    /// Whether `file` declares a namespace and none of them is `expected`
    /// (`.`-joined; empty for the global namespace).
    pub(crate) fn declares_other_namespace(&self, file: &str, expected: &str) -> bool {
        self.file_namespaces
            .get(file)
            .is_some_and(|declared| declared.iter().all(|ns| ns != expected))
    }

    /// Files declaring `namespace` (`.`-joined), see `namespace_files`.
    pub(crate) fn namespace_files(&self, namespace: &str) -> &[String] {
        self.namespace_files
            .get(namespace)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn module_files_by_stem<'a>(&'a self, stem: &str) -> impl Iterator<Item = &'a str> {
        self.files_by_stem(stem)
            .iter()
            .chain(self.empty_stem_index.get(stem).into_iter().flatten())
            .map(String::as_str)
    }

    pub(crate) fn has_module_name(&self, language: Language, name: &str) -> bool {
        self.indexed_module_names
            .get(&std::mem::discriminant(&language))
            .is_some_and(|names| names.contains(name))
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

        let file_meta = FileMeta::from_path(file_path);
        self.node_kinds.push(kind);
        self.node_file_meta.push(file_meta);
        self.node_owner.push(NO_OWNER);
        self.note_constructible(node_name, kind);
        self.note_namespace(file_path, file_meta.language, node_name, kind);
    }

    fn note_constructible(&mut self, node_name: &str, kind: NodeKind) {
        if kind.is_constructible() && !self.constructible_names.contains(node_name) {
            self.constructible_names.insert(node_name.into());
        }
        if kind == NodeKind::EnumVariant && !self.enum_variant_names.contains(node_name) {
            self.enum_variant_names.insert(node_name.into());
        }
    }

    fn note_namespace(
        &mut self,
        file_path: &str,
        language: Language,
        node_name: &str,
        kind: NodeKind,
    ) {
        if matches!(kind, NodeKind::Namespace | NodeKind::Module) {
            if let Some(key) = owner_key(node_name) {
                self.namespace_decls.push((file_path.into(), key.into()));
            }
        }
        if kind != NodeKind::Namespace || !fqn_language(language) {
            return;
        }
        let namespace = node_name.replace('\\', ".");
        let declared = self
            .file_namespaces
            .entry(file_path.to_string())
            .or_default();
        if !declared.contains(&namespace) {
            declared.push(namespace.clone());
        }
        let files = self.namespace_files.entry(namespace).or_default();
        // Pass 1 registers a file's nodes together, so a second declaration
        // of the same namespace in one file is adjacent.
        if files.last().map(String::as_str) != Some(file_path) {
            files.push(file_path.to_string());
        }
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
        self.owner_keys.push(key.into());
        id
    }

    /// Record the return type the function `node_id` declares, as written
    /// (`RawFunctionMeta.return_type`). Only a type [`return_type_key`]
    /// reduces to a bare name is kept, interned once per distinct name.
    pub fn note_return_type(&mut self, node_id: u32, return_type: &str) {
        let Some(key) = return_type_key(return_type) else {
            return;
        };
        let id = match self.return_type_ids.get(key) {
            Some(&id) => id,
            None => {
                let id = self.return_type_keys.len() as u32;
                self.return_type_ids.insert(key.into(), id);
                self.return_type_keys.push(key.into());
                id
            }
        };
        self.return_types.insert(node_id, id);
    }

    /// The [`return_type_key`] the function `node_id` declares.
    pub fn return_type_of(&self, node_id: u32) -> Option<&str> {
        self.return_types
            .get(&node_id)
            .map(|&id| &*self.return_type_keys[id as usize])
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
            self.constructors.push(node_id);
        }
        self.note_constructible(node_name, kind);
        self.note_namespace(file_path, file_meta.language, node_name, kind);
    }

    /// Map every Class / Struct to its one constructor: the Constructor in
    /// the type's own file whose owner key is the type's name. A type whose
    /// file holds several (two same-named types, each with a constructor)
    /// maps to none, and its constructions land on the type. Overloads are
    /// not several: Pass 1 registers only the first of them by name
    /// ([`Self::register_overload`]), which then stands for "a constructor of
    /// the type". The query-time overlay applies the same rule. Call once
    /// after Pass 1.
    pub fn build_constructor_index(&mut self) {
        let mut owner_names: Vec<&str> = vec![""; self.owner_ids.len()];
        for (key, &id) in &self.owner_ids {
            owner_names[id as usize] = key;
        }
        let mut picks: FxHashMap<u32, Option<u32>> = FxHashMap::default();
        for &ctor in &self.constructors {
            let owner = owner_names[self.node_owner[ctor as usize] as usize];
            let Some(types) = self
                .id_to_file
                .get(&ctor)
                .and_then(|file| self.file_scoped.get(file))
                .and_then(|names| names.get(owner))
            else {
                continue;
            };
            for &ty in types {
                if self.node_kinds[ty as usize].is_constructible() {
                    picks
                        .entry(ty)
                        .and_modify(|pick| *pick = None)
                        .or_insert(Some(ctor));
                }
            }
        }
        self.sole_constructors = picks
            .into_iter()
            .filter_map(|(ty, ctor)| Some((ty, ctor?)))
            .collect();
    }

    /// Mark the owners that name a namespace or module rather than a type:
    /// per file, an owner key that a Namespace / Module declaration of the
    /// file carries and no type declaration of the file does. C++ declares
    /// `detail::to_json` with owner `detail`, C# a namespace's classes with
    /// owner `N1.N2`, Rust an inline `mod m`'s functions with owner `m`; such
    /// a declaration is free for the pick rules, not a member of a type named
    /// `detail`. An owner the file declares no namespace for (an out-of-line
    /// `K::m`, a foreign `impl T for Vec<u8>`) stays a type. Call once after
    /// Pass 1.
    pub fn build_owner_kinds(&mut self) {
        let mut namespace_owners: FxHashMap<Box<str>, Vec<u32>> = FxHashMap::default();
        for (file, key) in &self.namespace_decls {
            let Some(&owner) = self.owner_ids.get(key) else {
                continue;
            };
            let declares_type = self
                .file_scoped
                .get(&**file)
                .and_then(|names| names.get(&**key))
                .is_some_and(|ids| {
                    ids.iter().any(|&id| {
                        let kind = self.node_kinds[id as usize];
                        kind.is_type() || kind == NodeKind::Impl
                    })
                });
            if declares_type {
                continue;
            }
            let owners = namespace_owners.entry(file.clone()).or_default();
            if !owners.contains(&owner) {
                owners.push(owner);
            }
        }
        self.namespace_owners = namespace_owners;
    }

    /// Record that the Rust callable `member` is declared in the `impl`
    /// block `block` (any id unique to the block), which implements
    /// `implemented` (`None` for an inherent `impl`). The parser gives a
    /// member of `impl Trait for Cursor<T>` no owner, and the uid of a
    /// second such member then collides with the first, so Pass 1 files it
    /// as an overload; this record is how the pick rules still tell the
    /// blocks apart ([`Self::prefer_owned`]).
    pub fn note_impl_member(&mut self, member: u32, block: u32, implemented: Option<&str>) {
        let trait_owner = self.intern_owner(implemented);
        self.impl_members
            .insert(member, ImplMember { block, trait_owner });
    }

    /// Whether `id` has no owner and no `impl` block either.
    fn is_free(&self, id: u32) -> bool {
        self.node_owner[id as usize] == NO_OWNER && !self.impl_members.contains_key(&id)
    }

    /// Whether some declaration named `name` that `caller` admits is a
    /// `Method`: the name is a member of some type, so a call site that
    /// writes it bare may be a member call whose receiver the parser
    /// dropped (`Resolver::resolve_call`).
    pub(crate) fn names_admitted_method(&self, name: &str, caller: FileMeta) -> bool {
        self.global_scoped.get(name).is_some_and(|ids| {
            ids.iter().any(|&id| {
                self.node_kinds[id as usize] == NodeKind::Method
                    && caller.admits(self.node_file_meta[id as usize])
            })
        })
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

    /// Register the next id as a later overload of `first`
    /// ([`ecp_core::uid::keeps_overloads`]): same kind and owner, but in no
    /// name table, so every name lookup still returns `first`. Which overload
    /// a call means needs argument types the parse does not carry; the
    /// builder links `first` to the rest instead ([`Self::overload_sets`]).
    /// Returns the overload's 1-based ordinal.
    pub fn register_overload(&mut self, first: u32, file_meta: FileMeta) -> u32 {
        let id = self.node_kinds.len() as u32;
        self.node_kinds.push(self.node_kinds[first as usize]);
        self.node_file_meta.push(file_meta);
        self.node_owner.push(self.node_owner[first as usize]);
        let later = self.overloads.entry(first).or_default();
        later.push(id);
        later.len() as u32
    }

    /// The later overloads of `first`, in source order.
    pub fn later_overloads(&self, first: u32) -> &[u32] {
        self.overloads.get(&first).map_or(&[], Vec::as_slice)
    }

    /// Every overload set as (first, later overloads), by first id.
    pub fn overload_sets(&self) -> impl Iterator<Item = (u32, &[u32])> {
        let mut firsts: Vec<u32> = self.overloads.keys().copied().collect();
        firsts.sort_unstable();
        firsts
            .into_iter()
            .map(|first| (first, self.later_overloads(first)))
    }

    /// The first declaration in `file_path` named `node_name` with `kind` and
    /// `owner`: the id its later overloads hang off.
    pub fn first_declaration(
        &self,
        file_path: &str,
        node_name: &str,
        kind: NodeKind,
        owner: Option<&str>,
    ) -> Option<u32> {
        let owner = match owner.and_then(owner_key) {
            None => NO_OWNER,
            Some(key) => *self.owner_ids.get(key)?,
        };
        self.file_scoped
            .get(file_path)?
            .get(node_name)?
            .iter()
            .copied()
            .find(|&id| {
                self.node_kinds[id as usize] == kind && self.node_owner[id as usize] == owner
            })
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

    /// Whether `file_path` or a file in its directory declares a type named
    /// `node_name`.
    pub(crate) fn declares_type_beside(&self, file_path: &str, node_name: &str) -> bool {
        if self
            .lookup_in_file_with_kind(file_path, node_name, ResolveTarget::Type)
            .is_some()
        {
            return true;
        }
        let dir = std::path::Path::new(file_path).parent();
        let predicate = ResolveTarget::Type.kind_predicate();
        self.global_scoped.get(node_name).is_some_and(|ids| {
            ids.iter().any(|&id| {
                predicate(self.node_kinds[id as usize])
                    && self
                        .file_of(id)
                        .map(|file| std::path::Path::new(file).parent())
                        == Some(dir)
            })
        })
    }

    /// [`Self::lookup_in_file_with_kind`] for the bare name of a call site,
    /// through the shared [`pick_call`] rule. The pick is by source order
    /// only; a call site narrows it by owner through [`Self::prefer_owned`].
    pub fn lookup_call_in_file(
        &self,
        file_path: &str,
        node_name: &str,
        target: ResolveTarget,
    ) -> Option<u32> {
        let ids = self.file_scoped.get(file_path)?.get(node_name)?;
        pick_call(
            ids.iter().map(|&id| (id, self.node_kinds[id as usize])),
            target,
        )
    }

    /// `file_path`'s declarations of `node_name` in source order, with their
    /// kinds: what the shared import rules read
    /// ([`ecp_core::analyzer::reexport::ModuleFacts::decls_in_file`]).
    pub(crate) fn decls_in_file(&self, file_path: &str, node_name: &str) -> Vec<(u32, NodeKind)> {
        self.file_scoped
            .get(file_path)
            .and_then(|names| names.get(node_name))
            .map_or_else(Vec::new, |ids| {
                ids.iter()
                    .map(|&id| (id, self.node_kinds[id as usize]))
                    .collect()
            })
    }

    /// Record that `node_id`, declared in `file_path`, is exported. A TS / JS
    /// file with an exported declaration is an ES module: another file
    /// reaches only its exports. A file with none (CommonJS, a script) and a
    /// `.d.ts`, whose declarations TS may export implicitly, prove nothing,
    /// and other languages bind imports by their own rules.
    pub(crate) fn note_export(&mut self, file_path: &str, language: Language, node_id: u32) {
        if !is_es_module(language, file_path, true, false) {
            return;
        }
        self.exported_ids.insert(node_id);
        if !self.es_modules.contains(file_path) {
            self.es_modules.insert(file_path.into());
        }
    }

    /// Record the export names of `file_path` that the parser pairs with
    /// their declarations ([`RawExportAlias`]). Each makes the file an ES
    /// module, as an exported declaration does ([`Self::note_export`]).
    pub(crate) fn note_export_aliases(
        &mut self,
        file_path: &str,
        language: Language,
        aliases: &[RawExportAlias],
    ) {
        let Some(table) = ExportAliasTable::build(
            aliases
                .iter()
                .map(|alias| (&*alias.exported, &*alias.local)),
        )
        .filter(|_| is_es_module(language, file_path, false, true)) else {
            return;
        };
        if !self.es_modules.contains(file_path) {
            self.es_modules.insert(file_path.into());
        }
        self.export_aliases.insert(file_path.into(), table);
    }

    /// Whether `file_path` is a TS / JS file that exports a declaration, so
    /// another module reaches only its exports.
    pub(crate) fn is_es_module(&self, file_path: &str) -> bool {
        self.es_modules.contains(file_path)
    }

    /// The export alias table of the ES module `file_path`, if it has one.
    pub(crate) fn export_aliases(&self, file_path: &str) -> Option<&ExportAliasTable> {
        self.export_aliases.get(file_path)
    }

    /// Whether `node_id` is an exported declaration of an ES module.
    pub(crate) fn is_exported(&self, node_id: u32) -> bool {
        self.exported_ids.contains(&node_id)
    }

    /// Every exported declaration of an ES module, in no order.
    pub(crate) fn exported_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.exported_ids.iter().copied()
    }

    /// Whether `id` is declared outside any owning type. A method of a Rust
    /// `impl` block the parser gave no owner is not: `use m::f` never names it.
    pub(crate) fn is_top_level(&self, id: NodeId) -> bool {
        self.is_free(id)
    }

    /// [`Self::lookup_call_in_file`] narrowed by the owner an import binding
    /// requires: [`MemberOwner::Any`] takes the match [`Self::prefer_owned`]
    /// picks with no owner (a free function, else one type's member),
    /// `TopLevel` a declaration with no owning type, `Type` a member of the
    /// type its path names, when the file's types agree with that path.
    pub(crate) fn lookup_member_in_file(
        &self,
        file_path: &str,
        node_name: &str,
        target: ResolveTarget,
        owner: MemberOwner<'_>,
    ) -> Option<u32> {
        let owner_id = match owner {
            MemberOwner::Any => {
                return self
                    .lookup_call_in_file(file_path, node_name, target)
                    .and_then(|pick| {
                        self.prefer_owned(
                            file_path,
                            pick,
                            node_name,
                            None,
                            &[],
                            target,
                            PickScope::default(),
                        )
                    })
            }
            MemberOwner::TopLevel => NO_OWNER,
            MemberOwner::Type(path) if !self.owner_path_agrees(file_path, path) => return None,
            MemberOwner::Type(path) => self.owner_id(path)?,
        };
        let predicate = target.kind_predicate();
        self.file_scoped
            .get(file_path)?
            .get(node_name)?
            .iter()
            .copied()
            .find(|&id| {
                self.node_owner[id as usize] == owner_id && predicate(self.node_kinds[id as usize])
            })
    }

    /// [`owner_path_agrees`] over the types `file_path` declares.
    pub(crate) fn owner_path_agrees(&self, file_path: &str, path: &str) -> bool {
        let names = self.file_scoped.get(file_path);
        let is_type = ResolveTarget::Type.kind_predicate();
        owner_path_agrees(
            path,
            |name| {
                let mut types = names
                    .and_then(|m| m.get(name))
                    .into_iter()
                    .flatten()
                    .filter(|&&id| is_type(self.node_kinds[id as usize]));
                match (types.next(), types.next()) {
                    (None, _) => DeclaredType::Undeclared,
                    (Some(_), Some(_)) => DeclaredType::Ambiguous,
                    (Some(&id), None) => DeclaredType::Unique(
                        Some(self.node_owner[id as usize]).filter(|&owner| owner != NO_OWNER),
                    ),
                }
            },
            |segment| self.owner_id(segment),
        )
    }

    /// Tier-3 global lookup: kind-filtered same-name candidates through the
    /// shared barrier filter, [`pick_global`]. `free_only` is a bare `f()`
    /// that never reaches a member ([`global_field`]).
    pub fn lookup_global(
        &self,
        node_name: &str,
        target: ResolveTarget,
        caller: FileMeta,
        free_only: bool,
    ) -> GlobalPick {
        let Some(raw) = self.global_scoped.get(node_name) else {
            return GlobalPick::NoMatch;
        };
        let field = global_field(target, free_only);
        let pick = pick_global(
            caller,
            raw.iter()
                .filter(|&&id| field(self.node_kinds[id as usize]))
                .map(|&id| (id, self.node_file_meta[id as usize])),
        );
        match pick {
            GlobalPick::Unique(id) if !target.kind_predicate()(self.node_kinds[id as usize]) => {
                GlobalPick::NoMatch
            }
            other => other,
        }
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

    /// The declaration a call of `member` reaches among `file_path`'s
    /// same-named declarations of `target`'s kinds, given `pick`, the first
    /// of them in source order: the shared rule
    /// [`same_file_pick::prefer_owned`], over this table.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prefer_owned(
        &self,
        file_path: &str,
        pick: u32,
        member: &str,
        owner: Option<&str>,
        heritage: &[String],
        target: ResolveTarget,
        scope: PickScope,
    ) -> Option<u32> {
        same_file_pick::prefer_owned(
            self, file_path, pick, member, owner, heritage, target, scope,
        )
    }

    /// Whether an untyped member call may bind `id`
    /// ([`same_file_pick::member_call_admits`]).
    pub(crate) fn member_call_admits(&self, id: u32, caller_owner: Option<&str>) -> bool {
        same_file_pick::member_call_admits(self, id, caller_owner)
    }

    /// The candidate nearest `caller_file` when `candidates` are copies of
    /// one declaration ([`Self::is_copy`]). Duplicated source trees
    /// (`android/` beside the main tree, vendored copies, two packages
    /// publishing one module) hold such copies; a build compiles the caller
    /// against one of them, and the copy that shares the deepest directory
    /// with the caller is the one its own tree holds. Fewer directory steps
    /// between the two break an equal depth. `None` for a tie, for fewer
    /// than two candidates, or when any candidate is a different declaration.
    pub(crate) fn nearest_copy(&self, caller_file: &str, candidates: &[u32]) -> Option<u32> {
        let (&first, rest) = candidates.split_first()?;
        if rest.is_empty() || !rest.iter().all(|&id| self.is_copy(first, id)) {
            return None;
        }
        let caller_dirs = path_dirs(caller_file);
        let mut best: Option<((usize, std::cmp::Reverse<usize>), u32)> = None;
        let mut tie = false;
        for &id in candidates {
            let candidate_dirs = path_dirs(self.file_of(id)?);
            let shared = caller_dirs
                .iter()
                .zip(&candidate_dirs)
                .take_while(|(a, b)| a == b)
                .count();
            let steps = caller_dirs.len() + candidate_dirs.len() - 2 * shared;
            let key = (shared, std::cmp::Reverse(steps));
            match best {
                Some((best_key, _)) if key < best_key => {}
                Some((best_key, _)) if key == best_key => tie = true,
                _ => {
                    best = Some((key, id));
                    tie = false;
                }
            }
        }
        best.filter(|_| !tie).map(|(_, id)| id)
    }

    /// [`Self::nearest_copy`] over the set [`Self::lookup_global`] found
    /// ambiguous: the same kind filter and [`FileMeta::admits`] barrier. The
    /// walk stops at the first candidate that is not a copy, so a common
    /// name with many unrelated declarations costs two comparisons.
    pub(crate) fn nearest_global_copy(
        &self,
        caller_file: &str,
        caller: FileMeta,
        node_name: &str,
        target: ResolveTarget,
        free_only: bool,
    ) -> Option<u32> {
        let field = global_field(target, free_only);
        let mut copies: Vec<u32> = Vec::new();
        for &id in self.global_scoped.get(node_name)? {
            if !field(self.node_kinds[id as usize])
                || !caller.admits(self.node_file_meta[id as usize])
            {
                continue;
            }
            if copies
                .first()
                .is_some_and(|&first| !self.is_copy(first, id))
            {
                return None;
            }
            copies.push(id);
        }
        self.nearest_copy(caller_file, &copies)
            .filter(|&id| target.kind_predicate()(self.node_kinds[id as usize]))
    }

    /// Whether `b` repeats declaration `a` in another tree: the same kind
    /// and owner, in a file of the same name in a directory of the same name
    /// (Java's package directory, a Python package), declaring the same
    /// namespaces (a Kotlin `package`, a C# / PHP `namespace`). Two
    /// `utils.py` files of unrelated packages `a/` and `b/` are not copies.
    pub(crate) fn is_copy(&self, a: u32, b: u32) -> bool {
        let (Some(a_file), Some(b_file)) = (self.file_of(a), self.file_of(b)) else {
            return false;
        };
        a_file != b_file
            && self.node_kinds[a as usize] == self.node_kinds[b as usize]
            && self.node_owner[a as usize] == self.node_owner[b as usize]
            && leaf_path(a_file) == leaf_path(b_file)
            && self.file_namespaces.get(a_file) == self.file_namespaces.get(b_file)
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
        self.constructible_names.contains(name)
    }

    /// True when some `EnumVariant` is named `name`.
    pub fn has_enum_variant(&self, name: &str) -> bool {
        self.enum_variant_names.contains(name)
    }

    /// The variant `variant` of the type `type_id`: the `EnumVariant` in the
    /// type's file whose owner is the type. `type_name` is the name the
    /// reference used for the type; an import alias (`use E as F`) differs
    /// from the declared name, which is then looked up.
    pub fn enum_variant_of(&self, type_id: u32, type_name: &str, variant: &str) -> Option<u32> {
        let file = self.id_to_file.get(&type_id)?;
        let candidates = self.file_scoped.get(file)?.get(variant)?;
        let owned_by = |owner: &str| {
            let owner = self.owner_id(owner)?;
            candidates.iter().copied().find(|&id| {
                self.node_kinds[id as usize] == NodeKind::EnumVariant
                    && self.node_owner[id as usize] == owner
            })
        };
        owned_by(type_name).or_else(|| owned_by(self.name_in_file(type_id)?))
    }

    /// The variant a bare name (`RED`, a Java `case MONDAY:`) denotes: the
    /// one `file` declares, else the one value of that name the caller's
    /// barrier admits project-wide when that value is a variant, and whether
    /// it is in `file`. `None` when `file` declares something else of that
    /// name (a local `#define` shadows the enumerator), or another value
    /// (a macro or constant in some header) shares the name.
    pub fn bare_enum_variant(
        &self,
        file: &str,
        name: &str,
        caller: FileMeta,
    ) -> Option<(u32, bool)> {
        let is_variant = |id: u32| self.node_kinds[id as usize] == NodeKind::EnumVariant;
        if let Some(ids) = self.file_scoped.get(file).and_then(|names| names.get(name)) {
            return match ids.as_slice() {
                [id] if is_variant(*id) => Some((*id, true)),
                _ => None,
            };
        }
        let values = self.global_scoped.get(name)?.iter().copied().filter(|&id| {
            matches!(
                self.node_kinds[id as usize],
                NodeKind::EnumVariant | NodeKind::Macro | NodeKind::Const | NodeKind::Variable
            )
        });
        match pick_global(
            caller,
            values.map(|id| (id, self.node_file_meta[id as usize])),
        ) {
            GlobalPick::Unique(id) if is_variant(id) => Some((id, false)),
            _ => None,
        }
    }

    /// The one constructor a construction of type `type_id` calls; `None`
    /// when the type declares none or several (see
    /// [`Self::build_constructor_index`]).
    pub fn sole_constructor(&self, type_id: u32) -> Option<u32> {
        self.sole_constructors.get(&type_id).copied()
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

    /// True when at least one node is registered under `file_path`.
    pub fn has_file(&self, file_path: &str) -> bool {
        self.file_scoped
            .get(file_path)
            .is_some_and(|symbols| !symbols.is_empty())
    }

    /// Unlike `has_file`, includes empty and import-only files registered by the builder.
    pub(crate) fn has_indexed_file(&self, file_path: &str) -> bool {
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

    /// Every type with declared heritage and its supertypes, in unspecified
    /// order.
    pub fn all_supertypes(&self) -> impl Iterator<Item = (u32, &Supertypes)> {
        self.supertypes.iter().map(|(&ty, st)| (ty, st))
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

impl PickFacts for SymbolTable {
    fn pick_candidates(&self, file: &str, name: &str) -> Cow<'_, [u32]> {
        let Some(ids) = self.file_scoped.get(file).and_then(|names| names.get(name)) else {
            return Cow::Borrowed(&[]);
        };
        let impl_overloads = |id: u32| {
            self.later_overloads(id)
                .iter()
                .copied()
                .filter(|later| self.impl_members.contains_key(later))
        };
        if ids.iter().all(|&id| impl_overloads(id).next().is_none()) {
            return Cow::Borrowed(ids);
        }
        Cow::Owned(
            ids.iter()
                .flat_map(|&id| std::iter::once(id).chain(impl_overloads(id)))
                .collect(),
        )
    }

    fn kind(&self, id: u32) -> NodeKind {
        self.node_kind(id)
    }

    fn owner(&self, id: u32) -> Option<&str> {
        let owner = *self.node_owner.get(id as usize)?;
        (owner != NO_OWNER).then(|| &*self.owner_keys[owner as usize])
    }

    fn impl_member(&self, id: u32) -> Option<PickImplMember<'_>> {
        self.impl_members.get(&id).map(|member| PickImplMember {
            block: member.block,
            trait_owner: (member.trait_owner != NO_OWNER)
                .then(|| &*self.owner_keys[member.trait_owner as usize]),
        })
    }

    fn is_namespace_owner(&self, file: &str, owner: &str) -> bool {
        self.namespace_owners.get(file).is_some_and(|owners| {
            self.owner_ids
                .get(owner)
                .is_some_and(|id| owners.contains(id))
        })
    }

    fn file_of(&self, id: u32) -> Option<&str> {
        self.id_to_file.get(&id).map(String::as_str)
    }

    fn types_named(&self, owner: &str) -> Vec<u32> {
        self.global_scoped
            .get(owner)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&id| self.node_kinds[id as usize].is_type())
            .collect()
    }

    fn ancestors(&self, ty: u32) -> Vec<u32> {
        SymbolTable::ancestors(self, ty)
    }
}

impl TypeFacts for SymbolTable {
    fn supertypes(&self, ty: u32) -> Option<(Cow<'_, [u32]>, Option<u32>)> {
        self.supertypes
            .get(&ty)
            .map(|st| (Cow::Borrowed(&*st.bases), st.first_unresolved))
    }

    fn name_of(&self, id: u32) -> Option<&str> {
        self.name_in_file(id)
    }

    fn owned_in_file(
        &self,
        file: &str,
        member: &str,
        owner: &str,
        target: ResolveTarget,
    ) -> Vec<u32> {
        SymbolTable::owned_in_file(self, file, member, owner, target)
    }

    fn owned_global(
        &self,
        member: &str,
        owner: &str,
        target: ResolveTarget,
        scope_file: &str,
    ) -> Vec<u32> {
        SymbolTable::owned_global(self, member, owner, target, scope_file)
    }

    fn count_in_file(&self, file: &str, name: &str, target: ResolveTarget) -> usize {
        self.count_in_file_with_kind(file, name, target)
    }

    fn global_candidates(&self, name: &str, target: ResolveTarget, caller: FileMeta) -> Vec<u32> {
        SymbolTable::global_candidates(self, name, target, caller)
    }

    fn is_copy(&self, a: u32, b: u32) -> bool {
        SymbolTable::is_copy(self, a, b)
    }

    fn nearest_copy(&self, caller_file: &str, candidates: &[u32]) -> Option<u32> {
        SymbolTable::nearest_copy(self, caller_file, candidates)
    }

    fn crate_root<'p>(&self, path: &'p str) -> &'p str {
        crate_root_prefix(path)
    }

    fn return_type_of(&self, callee: u32) -> Option<&str> {
        SymbolTable::return_type_of(self, callee)
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

    /// A pick for a call from the declaration `caller`.
    fn from(caller: u32) -> PickScope {
        PickScope {
            caller: Some(caller),
            ..PickScope::default()
        }
    }

    /// A pick for an untyped `o.m()` from a member of `owner`'s type.
    const UNTYPED: PickScope = PickScope {
        caller: None,
        typed_receiver: false,
        untyped_receiver: true,
        qualifier_is_type: false,
    };

    /// A pick for a receiver-typed `K.m()`.
    const TYPED: PickScope = PickScope {
        caller: None,
        typed_receiver: true,
        untyped_receiver: false,
        qualifier_is_type: false,
    };

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
    fn test_lookup_member_in_file_type_path_checks_enclosing_types() {
        let st = st_owned(&[
            ("h/Outer1.java", "Outer2", NodeKind::Class, None),
            ("h/Outer1.java", "Helper", NodeKind::Class, Some("Outer2")),
            ("h/Outer1.java", "run", NodeKind::Method, Some("Helper")),
            ("h/Outer1.java", "Outer1", NodeKind::Class, None),
            ("r/Util.java", "Other", NodeKind::Class, None),
            ("r/Util.java", "run", NodeKind::Method, Some("Other")),
            ("r/Util.java", "Util", NodeKind::Class, None),
            ("r/Util.java", "run", NodeKind::Method, Some("Util")),
        ]);
        let lookup =
            |file, path| st.lookup_member_in_file(file, "run", CALL, MemberOwner::Type(path));
        assert_eq!(lookup("r/Util.java", "r.Util"), Some(7));
        assert_eq!(lookup("h/Outer1.java", "h.Outer2.Helper"), Some(2));
        assert_eq!(
            lookup("h/Outer1.java", "h.Outer1.Helper"),
            None,
            "`Helper` is nested in `Outer2`, not in the imported `Outer1`"
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
    fn test_prefer_owned_unrelated_first_pick_returns_own_base_or_none() {
        let st = st_owned(&[
            ("a.java", "save", NodeKind::Method, Some("A")),
            ("a.java", "save", NodeKind::Method, Some("B<T>")),
            ("a.java", "save", NodeKind::Method, Some("K")),
            ("a.java", "run", NodeKind::Method, Some("C")),
        ]);
        let pick = |owner, heritage: &[String]| {
            st.prefer_owned(
                "a.java",
                0,
                "save",
                owner,
                heritage,
                CALL,
                PickScope::default(),
            )
        };
        assert_eq!(pick(Some("K"), &[]), Some(2), "the owner's own member");
        assert_eq!(
            pick(Some("C"), &["pkg.B<String>".to_string()]),
            Some(1),
            "a base named by a qualified, generic path"
        );
        assert_eq!(
            pick(Some("C"), &["Missing".to_string(), "B".to_string()]),
            Some(1),
            "the first base that owns a member"
        );
        assert_eq!(pick(Some("C"), &[]), None, "three unrelated owners");
        assert_eq!(pick(None, &[]), None, "a caller with no owner");
    }

    #[test]
    fn test_prefer_owned_free_function_or_one_owner_returns_it() {
        let st = st_owned(&[
            ("a.rs", "save", NodeKind::Method, Some("A")),
            ("a.rs", "save", NodeKind::Function, None),
            ("b.rs", "save", NodeKind::Method, Some("A")),
            ("b.rs", "save", NodeKind::Method, Some("A<T>")),
        ]);
        assert_eq!(
            st.prefer_owned(
                "a.rs",
                0,
                "save",
                Some("D"),
                &[],
                CALL,
                PickScope::default()
            ),
            Some(1),
            "a member of an unrelated type never beats a free function"
        );
        assert_eq!(
            st.prefer_owned(
                "a.rs",
                1,
                "save",
                Some("A"),
                &[],
                CALL,
                PickScope::default()
            ),
            Some(1),
            "a free function pick stays"
        );
        assert_eq!(
            st.prefer_owned("b.rs", 2, "save", None, &[], CALL, PickScope::default()),
            Some(2),
            "one owner declared in two blocks counts once"
        );
    }

    #[test]
    fn test_prefer_owned_trait_and_its_impls_returns_trait_declaration() {
        let rows = [
            ("lib.rs", "Foo", NodeKind::Struct, None),
            ("lib.rs", "handle", NodeKind::Method, Some("Foo")),
            ("lib.rs", "Handler", NodeKind::Trait, None),
            ("lib.rs", "handle", NodeKind::Method, Some("Handler")),
            ("lib.rs", "Bar", NodeKind::Struct, None),
            ("lib.rs", "handle", NodeKind::Method, Some("Bar")),
        ];
        let mut st = st_owned(&rows);
        assert_eq!(
            st.prefer_owned("lib.rs", 1, "handle", None, &[], CALL, PickScope::default()),
            None,
            "unrelated owners before the supertypes pre-pass"
        );
        st.build_supertypes([(0, Some(2)), (4, Some(2))]);
        assert_eq!(
            st.prefer_owned("lib.rs", 1, "handle", None, &[], CALL, PickScope::default()),
            Some(3),
            "every impl inherits from `Handler`, declared after the first impl"
        );
        assert_eq!(
            st.prefer_owned(
                "lib.rs",
                1,
                "handle",
                Some("Bar"),
                &[],
                CALL,
                PickScope::default()
            ),
            Some(5),
            "the caller's own member still comes first"
        );
    }

    #[test]
    fn test_prefer_owned_namespace_member_counts_as_free_returns_it() {
        let mut st = st_owned(&[
            ("a.cpp", "A", NodeKind::Class, None),
            ("a.cpp", "to_json", NodeKind::Method, Some("A")),
            ("a.cpp", "detail", NodeKind::Namespace, None),
            ("a.cpp", "to_json", NodeKind::Function, Some("detail")),
        ]);
        assert_eq!(
            st.prefer_owned("a.cpp", 1, "to_json", None, &[], CALL, PickScope::default()),
            None,
            "before the owner kinds are built, `detail` reads as a type"
        );
        st.build_owner_kinds();
        assert_eq!(
            st.prefer_owned("a.cpp", 1, "to_json", None, &[], CALL, PickScope::default()),
            Some(3)
        );
        assert_eq!(
            st.prefer_owned(
                "a.cpp",
                1,
                "to_json",
                Some("A"),
                &[],
                CALL,
                PickScope::default()
            ),
            Some(1),
            "the caller's own member still comes first"
        );
    }

    #[test]
    fn test_build_owner_kinds_namespace_and_type_in_one_file_keeps_type() {
        let mut st = st_owned(&[
            ("a.cpp", "detail", NodeKind::Namespace, None),
            ("a.cpp", "detail", NodeKind::Class, None),
            ("a.cpp", "m", NodeKind::Method, Some("detail")),
            ("a.cpp", "m", NodeKind::Method, Some("B")),
            ("b.cpp", "detail", NodeKind::Namespace, None),
            ("b.cpp", "m", NodeKind::Function, Some("detail")),
            ("b.cpp", "m", NodeKind::Method, Some("B")),
        ]);
        st.build_owner_kinds();
        assert_eq!(
            st.prefer_owned("a.cpp", 2, "m", None, &[], CALL, PickScope::default()),
            None,
            "`a.cpp` declares a type `detail`: two unrelated types"
        );
        assert_eq!(
            st.prefer_owned("b.cpp", 6, "m", None, &[], CALL, PickScope::default()),
            Some(5),
            "`b.cpp` declares only the namespace `detail`"
        );
    }

    #[test]
    fn test_prefer_owned_nested_namespace_path_owner_counts_as_free() {
        let mut st = st_owned(&[
            ("a.cpp", "m", NodeKind::Method, Some("K")),
            ("a.cpp", "a::c", NodeKind::Namespace, None),
            ("a.cpp", "m", NodeKind::Function, Some("a::c")),
        ]);
        st.build_owner_kinds();
        assert_eq!(
            st.prefer_owned("a.cpp", 0, "m", None, &[], CALL, PickScope::default()),
            Some(2)
        );
    }

    const METHOD: ResolveTarget = ResolveTarget::Method;

    /// `trait T { fn m(&self); }`, then `impl T for Foreign` (an owner no
    /// type of the index names) and `impl T for Vec<u8>` (no owner).
    fn st_trait_with_foreign_impls() -> SymbolTable {
        let mut st = st_owned(&[
            ("lib.rs", "T", NodeKind::Trait, None),
            ("lib.rs", "m", NodeKind::Method, Some("T")),
            ("lib.rs", "m", NodeKind::Method, Some("Foreign")),
            ("lib.rs", "m", NodeKind::Method, None),
        ]);
        st.note_impl_member(2, 2, Some("T"));
        st.note_impl_member(3, 3, Some("T"));
        st.build_owner_kinds();
        st
    }

    #[test]
    fn test_prefer_owned_trait_and_foreign_impls_returns_trait_declaration() {
        let st = st_trait_with_foreign_impls();
        assert_eq!(
            st.prefer_owned("lib.rs", 1, "m", None, &[], METHOD, PickScope::default()),
            Some(1)
        );
        assert_eq!(
            st.prefer_owned("lib.rs", 3, "m", None, &[], METHOD, PickScope::default()),
            Some(1),
            "an ownerless impl member is no free function"
        );
    }

    #[test]
    fn test_is_top_level_ownerless_impl_member_returns_false() {
        let mut st = st_owned(&[
            ("lib.rs", "f", NodeKind::Function, None),
            ("lib.rs", "m", NodeKind::Method, None),
        ]);
        st.note_impl_member(1, 1, Some("T"));
        assert!(st.is_top_level(0));
        assert!(!st.is_top_level(1));
    }

    #[test]
    fn test_prefer_owned_impl_of_other_trait_returns_none() {
        let mut st = st_trait_with_foreign_impls();
        st.note_impl_member(3, 3, Some("Other"));
        assert_eq!(
            st.prefer_owned("lib.rs", 1, "m", None, &[], METHOD, PickScope::default()),
            None
        );
    }

    #[test]
    fn test_prefer_owned_caller_in_impl_block_returns_own_block_member() {
        let mut st = st_owned(&[
            ("aw.rs", "AsyncWrite", NodeKind::Trait, None),
            ("aw.rs", "flush", NodeKind::Method, Some("AsyncWrite")),
            ("aw.rs", "flush", NodeKind::Method, None),
            ("aw.rs", "shutdown", NodeKind::Method, None),
        ]);
        let meta = FileMeta::from_path("aw.rs");
        // The second block's members collide with the first's uids.
        let later_flush = st.register_overload(2, meta);
        let later_shutdown = st.register_overload(3, meta);
        assert_eq!((later_flush, later_shutdown), (1, 1));
        for (member, block) in [(2, 2), (3, 2), (4, 4), (5, 4)] {
            st.note_impl_member(member, block, Some("AsyncWrite"));
        }
        st.build_owner_kinds();
        assert_eq!(
            st.prefer_owned("aw.rs", 1, "flush", None, &[], METHOD, from(5)),
            Some(4),
            "the later block's own member, filed as an overload"
        );
        assert_eq!(
            st.prefer_owned("aw.rs", 1, "flush", None, &[], METHOD, from(3)),
            Some(2)
        );
        assert_eq!(
            st.prefer_owned("aw.rs", 1, "flush", None, &[], METHOD, from(4)),
            Some(1),
            "the caller is never its own block's pick: the trait declaration"
        );
        assert_eq!(
            st.prefer_owned("aw.rs", 1, "flush", None, &[], CALL, from(5)),
            Some(1),
            "a non-method call site ignores the block: the trait declaration"
        );
    }

    /// `size` free first, then `lexer::size`, then `string::size`.
    fn st_size() -> SymbolTable {
        st_owned(&[
            ("a.hpp", "size", NodeKind::Function, None),
            ("a.hpp", "size", NodeKind::Method, Some("lexer")),
            ("a.hpp", "size", NodeKind::Method, Some("string")),
        ])
    }

    #[test]
    fn test_prefer_owned_typed_receiver_with_free_function_first_returns_own_member() {
        let st = st_size();
        assert_eq!(
            st.prefer_owned("a.hpp", 0, "size", Some("lexer"), &[], CALL, TYPED),
            Some(1)
        );
        assert_eq!(
            st.prefer_owned(
                "a.hpp",
                0,
                "size",
                Some("lexer"),
                &[],
                CALL,
                PickScope::default()
            ),
            Some(0),
            "an implicit-this bare call keeps the free pick"
        );
        assert_eq!(
            st.prefer_owned("a.hpp", 0, "size", Some("mod"), &[], CALL, TYPED),
            Some(0),
            "a qualifier that owns no `size` names a module: the free function"
        );
    }

    #[test]
    fn test_prefer_owned_untyped_receiver_skips_free_and_own_type_returns_sole_other_member() {
        let st = st_size();
        assert_eq!(
            st.prefer_owned("a.hpp", 0, "size", Some("lexer"), &[], CALL, UNTYPED),
            None,
            "the caller's own `lexer` and `string`: the receiver may be either"
        );
        assert_eq!(
            st.prefer_owned("a.hpp", 1, "size", None, &[], CALL, UNTYPED),
            None,
            "two unrelated types, the free function never counts"
        );
        let other_only = st_owned(&[
            ("a.hpp", "size", NodeKind::Function, None),
            ("a.hpp", "size", NodeKind::Method, Some("string")),
            ("a.hpp", "run", NodeKind::Method, Some("lexer")),
        ]);
        assert_eq!(
            other_only.prefer_owned("a.hpp", 0, "size", Some("lexer"), &[], CALL, UNTYPED),
            Some(1),
            "one type declares `size`, and it is not the caller's"
        );
        let own_only = st_owned(&[
            ("a.hpp", "size", NodeKind::Function, None),
            ("a.hpp", "size", NodeKind::Method, Some("lexer")),
        ]);
        assert_eq!(
            own_only.prefer_owned("a.hpp", 0, "size", Some("lexer"), &[], CALL, UNTYPED),
            None,
            "neither the free function nor the caller's own member"
        );
    }

    #[test]
    fn test_member_call_admits_free_namespace_own_or_constructor_returns_false() {
        let mut st = st_owned(&[
            ("a.cpp", "size", NodeKind::Function, None),
            ("a.cpp", "size", NodeKind::Method, Some("lexer")),
            ("a.cpp", "size", NodeKind::Method, Some("string")),
            ("a.cpp", "detail", NodeKind::Namespace, None),
            ("a.cpp", "size", NodeKind::Function, Some("detail")),
            ("a.cpp", "size", NodeKind::Constructor, Some("size")),
        ]);
        st.build_owner_kinds();
        let admits: Vec<bool> = (0..6)
            .map(|id| st.member_call_admits(id, Some("lexer")))
            .collect();
        assert_eq!(admits, [false, false, true, false, false, false]);
        assert!(st.member_call_admits(1, None), "a caller with no type");
    }

    #[test]
    fn test_names_admitted_method_method_or_other_language_returns_flag() {
        let st = st_owned(&[
            ("a.hpp", "number_integer", NodeKind::Method, Some("sax")),
            ("t.cpp", "number_integer", NodeKind::Class, None),
            ("b.py", "widget", NodeKind::Method, Some("W")),
        ]);
        let cpp = FileMeta::from_path("p.hpp");
        assert!(st.names_admitted_method("number_integer", cpp));
        assert!(!st.names_admitted_method("widget", cpp));
        assert!(!st.names_admitted_method("missing", cpp));
    }

    #[test]
    fn test_lookup_member_in_file_any_owner_prefers_free_function_else_one_owner() {
        let st = st_owned(&[
            ("m.py", "save", NodeKind::Method, Some("A")),
            ("m.py", "save", NodeKind::Function, None),
            ("n.py", "save", NodeKind::Method, Some("A")),
            ("n.py", "save", NodeKind::Method, Some("B")),
            ("o.py", "save", NodeKind::Method, Some("A")),
        ]);
        let any = |file| st.lookup_member_in_file(file, "save", CALL, MemberOwner::Any);
        assert_eq!(any("m.py"), Some(1));
        assert_eq!(any("n.py"), None, "two unrelated owners");
        assert_eq!(any("o.py"), Some(4));
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
            st.lookup_global("Svc", ResolveTarget::Type, caller, false),
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
            st.lookup_global("Only", ResolveTarget::Type, caller, false),
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
