//! Symbol resolver — maps call-site names to global node ids.
//!
//! ## T1-6: FxHashMap uid lookup — decision record
//!
//! The roadmap item T1-6 asked whether a `FxHashMap<u64, NodeId>` field on
//! `SymbolTable` would yield a meaningful win.
//!
//! **`SymbolTable` does not hold `u64` uids at all.** The `uid: u64` hash
//! (xxh3-64 of kind + path + owner_class + name) lives in
//! `ZeroCopyGraph::Node.uid`, which is the serialised on-disk graph consulted
//! at *query time*. `SymbolTable` operates at *build time* with dense
//! sequential `NodeId (u32)` indices and is already O(1) for all its lookups
//! via `FxHashMap<String, …>`. Adding a `u64` uid field to `SymbolTable` would
//! require wiring uid computation into the builder before `current_node_idx`
//! is assigned, with no caller benefit — the resolver tiers never look up by
//! uid, only by (file_path, name) or (name) pairs.
//!
//! **The real O(N) gap** was in the query layer:
//! `impact::classify_symbol` (ecp-cli) did a linear scan over all graph nodes
//! per BFS caller entry to reverse-look-up `uid_string → node_idx`:
//!
//! ```text
//! graph.nodes.iter().enumerate()
//!     .find(|(_, n)| n.uid.to_native().to_string() == caller_uid)
//! ```
//!
//! This is O(N) per caller × O(symbols) per coverage analysis, which on a
//! 3k-node graph with 5 callers/symbol costs ~2 ms per call vs ~0.6 µs with
//! the FxHashMap path (>3000× speedup).
//!
//! **Fix**: `ecp_core::graph_query::build_uid_index` builds
//! `FxHashMap<u64, u32>` once per coverage-analysis call; callers are looked
//! up in O(1). `coverage_analyses` (impact.rs) now builds the table once and
//! passes it to `classify_symbol`.
//!
//! See `crates/ecp-analyzer/tests/resolver_fxhash_uid.rs` for the correctness
//! regression suite and `crates/ecp-analyzer/benches/resolver_lookup.rs` for
//! the before/after bench numbers.

use ecp_core::analyzer::types::RawImport;
use serde::Serialize;
use std::borrow::Cow;
use std::path::Path;
use std::sync::Mutex;

use crate::resolution::heuristics::ResolutionTier;
use crate::resolution::index::{
    crate_root_prefix, FileMeta, GlobalPick, Language, ResolveTarget, SymbolTable,
    MAX_HERITAGE_DEPTH,
};
use crate::resolution::path_aliases::PathAliases;
use crate::rust::module_tree::RustWorkspaceModTree;
use rustc_hash::FxHashSet;

pub type NodeId = u32;

#[cfg(not(windows))]
#[inline]
fn normalize_source_path(path: &Path) -> Cow<'_, str> {
    path.to_string_lossy()
}

#[cfg(windows)]
#[inline]
fn normalize_source_path(path: &Path) -> Cow<'_, str> {
    Cow::Owned(path.to_string_lossy().replace('\\', "/"))
}

/// Resolver outcome tier captured per `resolve_symbol` call when the dump
/// is enabled. Distinct from [`ResolutionTier`] because that enum models
/// only resolution *successes* (and has the unused `Fallback(...)` arm),
/// whereas the dump also needs to record the `Unresolved` outcome to let
/// the verification harness compute false negatives.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum DecisionTier {
    SameFile,
    ImportScoped,
    /// Tier 2.5 — qualifier-scoped lookup succeeded (see
    /// [`ResolutionTier::QualifierScoped`]).
    QualifierScoped,
    /// Tier 2.75 — heritage-scoped lookup succeeded (see
    /// [`ResolutionTier::HeritageScoped`]).
    HeritageScoped,
    Global,
    /// Tier 3 produced ≥2 kind-filtered candidates and suppressed the edge.
    /// Distinct from `Unresolved` (=0 candidates) so the verification
    /// harness can tell "no defence needed" from "defence fired" without
    /// needing to inspect `alt_count`. Edge behaviour is unchanged — both
    /// outcomes emit no edge.
    AmbiguousGlobal,
    /// Tier 3.5 — Rust workspace module-tree resolved the FQN path to a
    /// concrete file and found the member there. Confidence 1.0; tagged
    /// `reason: "module-tree"` so analytics can distinguish from Tier-4
    /// heuristic edges (confidence 0.7).
    ModuleTree,
    Unresolved,
    /// Receiver-typing ladder: the qualifier names one project type, and
    /// that type owns the member (in its file, or for Rust impls / Go
    /// methods / C# partials / Swift extensions, in its crate or package).
    TypeOwned,
    /// Receiver-typing ladder: a resolved declared supertype of the
    /// qualifier's one project type owns the member.
    TypeHeritage,
    /// Receiver-typing ladder: the qualifier names several project types;
    /// exactly one owns the member and every other provably does not.
    TypeCandidates,
}

/// One resolver attempt, captured when the dump buffer is enabled. The
/// builder serializes a sibling JSONL view of these (resolving
/// `target_id → target_file` via [`SymbolTable::file_of`]) — see
/// `docs/specs/2026-05-15-resolver-oracle-harness.md`.
#[derive(Debug, Clone, Serialize)]
pub struct ResolverDecision {
    pub src_file: String,
    pub name: String,
    pub specifier: Option<String>,
    pub tier: DecisionTier,
    pub target_id: Option<NodeId>,
    pub alt_count: u32,
    pub confidence: Option<f32>,
}

/// The core resolver engine that matches symbol names to concrete global nodes.
pub struct Resolver<'a> {
    symbol_table: &'a SymbolTable,
    /// `None` on the production path → zero-cost (single Option-discriminant
    /// branch in `record`, no `Mutex` touch). `Some(_)` only when the
    /// builder enabled dumping via [`Resolver::enable_dump`].
    // `Mutex` (not `RefCell`) so the whole `Resolver` is `Sync` and can be
    // shared across rayon workers. In the production path `decisions` is
    // `None` and the `Option::Some` guard in `record()` short-circuits
    // before any lock — Mutex overhead is paid only when --dump-resolver
    // is on (a debug-only flag, currently no-op in v2 layout).
    decisions: Option<Mutex<Vec<ResolverDecision>>>,
    /// Module-specifier aliases sourced from project config (TS
    /// `tsconfig.json` `compilerOptions.paths`, etc.). Consulted during
    /// Tier 2 import resolution before the relative-resolution fallback so
    /// `@/utils` maps to `src/utils` (then existing extension/index
    /// probing finishes the lookup).
    path_aliases: PathAliases,
    /// Rust workspace module tree for Tier 3.5 FQN resolution.
    /// `None` when not available (non-Rust repos, no Cargo.toml, build
    /// failure). Shared across rayon workers — read-only after construction.
    mod_tree: Option<&'a RustWorkspaceModTree>,
    /// Workspace root path, used by Tier 3.5 to make absolute paths
    /// repo-relative. `None` when `mod_tree` is `None`.
    workspace_root: Option<std::path::PathBuf>,
}

impl<'a> Resolver<'a> {
    /// Creates a new `Resolver` with a reference to the global `SymbolTable`.
    pub fn new(symbol_table: &'a SymbolTable) -> Self {
        Self {
            symbol_table,
            decisions: None,
            path_aliases: PathAliases::new(),
            mod_tree: None,
            workspace_root: None,
        }
    }

    /// Replace the resolver's empty default alias set. Used by the builder
    /// to forward project-level config (`tsconfig.json` etc.) into the
    /// Tier-2 specifier expansion.
    pub fn with_path_aliases(mut self, aliases: PathAliases) -> Self {
        self.path_aliases = aliases;
        self
    }

    /// Attach the Rust workspace module tree for Tier 3.5 FQN resolution.
    pub fn with_mod_tree(
        mut self,
        tree: &'a RustWorkspaceModTree,
        workspace_root: std::path::PathBuf,
    ) -> Self {
        self.mod_tree = Some(tree);
        self.workspace_root = Some(workspace_root);
        self
    }

    /// Turn on the decision recorder. Each subsequent `resolve_symbol` call
    /// pushes a [`ResolverDecision`] into the internal buffer.
    pub fn enable_dump(&mut self) {
        self.decisions = Some(Mutex::new(Vec::new()));
    }

    /// Drain the recorded decisions. Returns `None` if dumping was never
    /// enabled.
    pub fn take_decisions(&mut self) -> Option<Vec<ResolverDecision>> {
        self.decisions
            .take()
            .map(|m| m.into_inner().unwrap_or_default())
    }

    /// Enumerate candidate target file paths for an import specifier, walking
    /// the same expansion rules used internally by Tier 2 resolution
    /// (path-alias expansion, relative-resolution, Python-style dotted,
    /// extension/index suffix probing). The visitor is called once per
    /// candidate path string and may return `false` to stop early.
    ///
    /// Exposed for `post_process::imports_edges`, which needs to resolve
    /// module-style imports (e.g. Ruby `require_relative 'alpha'`, Go
    /// `import "x/pkg"`) to a File node target when no named symbol
    /// matches `RawImport.imported_name`.
    pub fn enumerate_candidates<F>(&self, source_file: &std::path::Path, specifier: &str, visit: F)
    where
        F: FnMut(&str) -> bool,
    {
        for_each_specifier_candidate(source_file, specifier, &self.path_aliases, visit);
    }

    /// Resolves a symbol name to possible target nodes with confidence scores.
    ///
    /// `target` constrains Tier-3 (Global) fallback so a bare `format()` /
    /// `new()` doesn't fan out to every same-named symbol in the graph.
    /// Tier-3 returns at most one match — ambiguity → zero edges.
    pub fn resolve_symbol(
        &self,
        source_file: &Path,
        symbol_name: &str,
        raw_imports: &[RawImport],
        target: ResolveTarget,
    ) -> Vec<(NodeId, f32)> {
        self.resolve_symbol_with_heritage(source_file, symbol_name, raw_imports, target, &[])
    }

    /// Resolve an explicit import without selecting an inaccessible local binding.
    pub fn resolve_imported_symbol(
        &self,
        source_file: &Path,
        symbol_name: &str,
        raw_imports: &[RawImport],
        target: ResolveTarget,
    ) -> Vec<(NodeId, f32)> {
        let source_file_str = normalize_source_path(source_file);
        let mut results = Vec::new();
        // Tier 2: Try ImportScoped (with L0 path normalization).
        //
        // The literal `import.source` is rarely a SymbolTable key on its own
        // — TS writes `./foo`, Python writes `.helpers`, etc., while
        // `SymbolTable.file_scoped` keys are repo-relative file paths like
        // `src/bar/foo.ts`. We expand each specifier into a small set of
        // candidate keys (relative-resolution + extension/index/__init__
        // guesses) and probe them in order.
        for import in raw_imports {
            let is_match = match &import.alias {
                Some(alias) => alias == symbol_name,
                None => import.imported_name == symbol_name,
            };

            if is_match {
                let exported_name = &import.imported_name;
                let mut hit: Option<NodeId> = None;
                for_each_specifier_candidate(
                    source_file,
                    &import.source,
                    &self.path_aliases,
                    |candidate| match self.symbol_table.lookup_in_file_with_kind(
                        candidate,
                        exported_name,
                        target,
                    ) {
                        Some(id) => {
                            hit = Some(id);
                            false // stop enumerating
                        }
                        None => true, // keep going
                    },
                );
                if let Some(node_id) = hit {
                    results.push((node_id, ResolutionTier::ImportScoped.base_confidence()));
                    self.record(
                        &source_file_str,
                        symbol_name,
                        Some(import.source.as_str()),
                        DecisionTier::ImportScoped,
                        Some(node_id),
                        0,
                        Some(ResolutionTier::ImportScoped.base_confidence()),
                    );
                    return results;
                }
            }
        }

        results
    }

    /// Variant that exposes the caller's enclosing-class heritage to enable
    /// Tier 2.75 (`HeritageScoped`). Production call edges should prefer this
    /// so cross-file mixin / inherited-method references resolve through
    /// `Bar extends Foo` / `class Bar; include Foo; end` without falling
    /// through to the strict Global tier.
    pub fn resolve_symbol_with_heritage(
        &self,
        source_file: &Path,
        symbol_name: &str,
        raw_imports: &[RawImport],
        target: ResolveTarget,
        caller_heritage: &[String],
    ) -> Vec<(NodeId, f32)> {
        let mut results = Vec::new();
        // Normalize path to use forward slashes to match indexed paths.
        let source_file_str = normalize_source_path(source_file);

        // Tier 1: Try SameFile (kind-aware so a property named `Foo` doesn't
        // win the lookup for a constructor call `Foo()` in the same file —
        // see `SymbolTable::file_scoped` doc).
        if let Some(node_id) =
            self.symbol_table
                .lookup_in_file_with_kind(&source_file_str, symbol_name, target)
        {
            results.push((node_id, ResolutionTier::SameFile.base_confidence()));
            self.record(
                &source_file_str,
                symbol_name,
                None,
                DecisionTier::SameFile,
                Some(node_id),
                0,
                Some(ResolutionTier::SameFile.base_confidence()),
            );
            return results; // Highest precedence, return early
        }

        let imported = self.resolve_imported_symbol(source_file, symbol_name, raw_imports, target);
        if !imported.is_empty() {
            return imported;
        }

        // Tier 2.5: Qualifier-scoped lookup. Callees that carry a qualifier
        // (`A::new`, `std::vec::Vec::new`, `Cls.method`) cannot match Tier 1/2
        // which are keyed by short names; without this tier they fall through
        // to Tier 3, where the kind+unique filter near-always rejects the
        // ultra-common member name (`new`, `default`, `from`, ...). Splitting
        // and scoping to the qualifier's defining file is the proper fix.
        //
        // No fall-through to Tier 3 on a short-name retry: a qualified callee
        // should resolve via its qualifier or not at all — matching the
        // "refuse to guess" principle that drives the Layer-1 barriers.
        //
        // Concretely, allowing a bare-name fallback would re-introduce a class
        // of pre-existing false edges that this tier was meant to remove:
        // `std::fs::read` stripping to `read` and resolving to a same-named
        // local function, `serde_json::json!` macro calls resolving to a
        // local `json()` helper, etc. Dump verification of B.1 vs B.1+fallback
        // showed a ~52% false-positive rate on the fallback path, so we keep
        // the strict policy. Module-qualified free functions like
        // `registry::sanitize_branch` whose member is uniquely defined will
        // be recovered by Phase B.3 (config-aware import resolution) once the
        // workspace crate index can distinguish `ecp_core::...` (internal,
        // safe to fall back) from `std::...` (external, refuse).
        if let Some((qualifier, member)) = split_qualifier(symbol_name) {
            let hit = self
                .resolve_qualifier_file(
                    source_file,
                    qualifier,
                    member,
                    target,
                    raw_imports,
                    Some(symbol_name),
                )
                .and_then(|qf| {
                    self.symbol_table
                        .lookup_in_file_with_kind(&qf, member, target)
                });
            if let Some(node_id) = hit {
                let conf = ResolutionTier::QualifierScoped.base_confidence();
                results.push((node_id, conf));
                self.record(
                    &source_file_str,
                    symbol_name,
                    None,
                    DecisionTier::QualifierScoped,
                    Some(node_id),
                    0,
                    Some(conf),
                );
                return results;
            }

            // Tier 3.5: Rust workspace module-tree FQN resolution.
            //
            // Fires when Tier 2.5 (qualifier-scoped) fails AND a Rust module
            // tree is available. Handles `crate::a::b::fn` and
            // `<crate_name>::a::b::fn` paths by walking the filesystem-backed
            // mod tree instead of relying on the qualifier-as-Type heuristic.
            //
            // Only fires for callee strings with `::` — dot-separated callees
            // are method calls (handled by heritage / Tier 2.5 via receiver
            // types) not module-path FQNs.
            if symbol_name.contains("::") {
                if let Some(node_id) =
                    self.try_module_tree_resolve(&source_file_str, symbol_name, member, target)
                {
                    const MT_CONF: f32 = 1.0;
                    results.push((node_id, MT_CONF));
                    self.record(
                        &source_file_str,
                        symbol_name,
                        None,
                        DecisionTier::ModuleTree,
                        Some(node_id),
                        0,
                        Some(MT_CONF),
                    );
                    return results;
                }
            }

            // Receiver-typing ladder: runs only where every tier above found
            // nothing, so it adds edges and never moves one.
            let tier = match self.resolve_member_via_type(
                source_file,
                &source_file_str,
                symbol_name,
                qualifier,
                member,
                target,
                raw_imports,
            ) {
                TypeMember::Edge(node_id, tier, conf) => {
                    results.push((node_id, conf));
                    self.record(
                        &source_file_str,
                        symbol_name,
                        None,
                        tier,
                        Some(node_id),
                        0,
                        Some(conf),
                    );
                    return results;
                }
                TypeMember::Ambiguous => DecisionTier::AmbiguousGlobal,
                TypeMember::NoEdge => DecisionTier::Unresolved,
            };
            self.record(
                &source_file_str,
                symbol_name,
                None,
                tier,
                None,
                self.symbol_table.global_match_count(member),
                None,
            );
            return results;
        }

        // Tier 2.75: HeritageScoped — bare-name callee in a class that
        // extends/includes/mixes in another type. Treat each parent name as
        // an implicit qualifier and probe the parent's defining file. This
        // is what makes `class Bar; include Foo; end` resolve a delegated
        // `read` defined inside `Foo` across files, and the same path serves
        // Java/Kotlin/C# subclasses calling inherited methods without `this.`.
        // Stops at the first hit (heritage order is the source-order list
        // recorded by the parser, mirroring MRO precedence).
        if !caller_heritage.is_empty() {
            for base in caller_heritage {
                if let Some(qf) = self.resolve_qualifier_file(
                    source_file,
                    base,
                    symbol_name,
                    target,
                    raw_imports,
                    None,
                ) {
                    if let Some(node_id) =
                        self.symbol_table
                            .lookup_in_file_with_kind(&qf, symbol_name, target)
                    {
                        let conf = ResolutionTier::HeritageScoped.base_confidence();
                        results.push((node_id, conf));
                        self.record(
                            &source_file_str,
                            symbol_name,
                            Some(base.as_str()),
                            DecisionTier::HeritageScoped,
                            Some(node_id),
                            0,
                            Some(conf),
                        );
                        return results;
                    }
                }
            }
        }

        // Tier 3: Global fallback — emit only when the kind-filtered candidate
        // set is unique. Refusing to guess on ambiguity is the dominant defence
        // against bare-name fan-out (`new`, `format`, `default`, `main`, ...).
        // `alt_count` in the dump still surfaces the raw same-name count so the
        // verification harness can distinguish suppressed-ambiguous from
        // truly-unresolved.
        let specifier = raw_imports
            .iter()
            .find(|i| match &i.alias {
                Some(a) => a == symbol_name,
                None => i.imported_name == symbol_name,
            })
            .map(|i| i.source.as_str());
        let raw_count = self.symbol_table.global_match_count(symbol_name);
        let caller_meta = FileMeta::from_path(&source_file_str);

        let (tier, target_id, confidence, alt_count) =
            match self
                .symbol_table
                .lookup_global(symbol_name, target, caller_meta)
            {
                GlobalPick::Unique(node_id) => {
                    let conf = ResolutionTier::Global.base_confidence();
                    results.push((node_id, conf));
                    (
                        DecisionTier::Global,
                        Some(node_id),
                        Some(conf),
                        raw_count.saturating_sub(1),
                    )
                }
                GlobalPick::Ambiguous => (DecisionTier::AmbiguousGlobal, None, None, raw_count),
                GlobalPick::NoMatch => (DecisionTier::Unresolved, None, None, raw_count),
            };
        self.record(
            &source_file_str,
            symbol_name,
            specifier,
            tier,
            target_id,
            alt_count,
            confidence,
        );

        results
    }
}

/// Split a qualified callee into `(qualifier, member)` where `qualifier` is
/// the **immediate** identifier preceding the rightmost separator. Returns
/// `None` if the name has no `::` / `.` separator or either side is empty.
///
/// For multi-segment paths only the last segment is taken as the qualifier
/// (the only piece the resolver can map back to a registered Type name —
/// `std::vec::Vec` is registered as just `Vec` keyed by its defining file).
///
/// Examples:
/// * `A::new` → `Some(("A", "new"))`
/// * `std::vec::Vec::new` → `Some(("Vec", "new"))`
/// * `obj.method` → `Some(("obj", "method"))`
/// * `foo` → `None`
///
/// Is the prefix preceding `qualifier` inside `full_callee` an "internal"
/// path (empty, or `crate` / `self` / `super` chain)? Tier 4 only fires on
/// calls where the qualifier names an internal module rather than the
/// trailing segment of an extern crate / std module path. `std::fs::read`
/// → preceding = `std` → returns false. `auto_ensure::ensure_fresh` →
/// preceding = `` → returns true. `crate::auto_ensure::ensure_fresh` →
/// preceding = `crate` → returns true.
fn qualifier_prefix_is_internal(full_callee: &str, qualifier: &str) -> bool {
    let Some(member_split) = full_callee.rfind("::").or_else(|| full_callee.rfind('.')) else {
        return false;
    };
    let before_member = &full_callee[..member_split];
    let preceding = before_member
        .rsplit_once("::")
        .or_else(|| before_member.rsplit_once('.'))
        .map(|(p, q)| if q == qualifier { p } else { "" })
        .unwrap_or("");
    preceding.is_empty()
        || preceding
            .split("::")
            .all(|s| matches!(s, "crate" | "self" | "super"))
}

/// For a Rust path call `a::b::Q::m`, does the module path `a::b` that the
/// call names for `Q` agree with the import of `Q` from `import_source`?
/// `de::Error::custom` with `use crate::error::Error` does not: `de::Error`
/// is another item than the imported `Error`. A head segment that a `use`
/// brings in (`use crate::error as err;`) expands to its full path first.
/// A `crate::` path must equal the import source; a relative one, with its
/// leading `self` / `super` dropped, must be a suffix of it. A call with no
/// module path before `Q`, or one not written with `::`, always agrees.
fn rust_path_prefix_agrees_with_import(
    full_callee: &str,
    qualifier: &str,
    import_source: &str,
    imports: &[RawImport],
) -> bool {
    let Some((before_member, _)) = full_callee.rsplit_once("::") else {
        return true;
    };
    let Some((prefix, q)) = before_member.rsplit_once("::") else {
        return true;
    };
    if q != qualifier {
        return true;
    }
    let mut named: Vec<&str> = prefix.split("::").collect();
    if let Some(module) = named.first().and_then(|&head| {
        imports
            .iter()
            .find(|i| i.alias.as_deref().unwrap_or(&i.imported_name) == head)
    }) {
        // `use crate::error as err;` records the whole path as the imported
        // name; `use crate::{error as err};` records source `crate`, name
        // `error`.
        let name = module.imported_name.as_str();
        let (path, tail) = if name.contains("::") {
            (name, None)
        } else {
            (module.source.as_str(), Some(name))
        };
        named.splice(..1, path.split("::").chain(tail));
    }
    // `crate::a::Q` names one module from the crate root: the import must
    // come from exactly that module. A relative path matches by suffix.
    if named.first() == Some(&"crate") {
        return import_source.split("::").eq(named);
    }
    let is_anchor = |s: &&str| matches!(*s, "" | "self" | "super");
    let named: Vec<&str> = named.into_iter().skip_while(is_anchor).collect();
    let source: Vec<&str> = import_source
        .split("::")
        .skip_while(|s| *s == "crate" || is_anchor(s))
        .collect();
    source.ends_with(&named)
}

/// A file that names its own directory's module: `mod.rs`, `lib.rs`,
/// `main.rs`, and every Cargo target root (`src/bin/<name>.rs`, top-level
/// `examples/`, `benches/`, `tests/` files, `build.rs`). Every other `.rs`
/// file is a module named after its stem. The top-level target rules require
/// no `src` ancestor, so `src/a/tests/x.rs` stays an ordinary module.
pub(crate) fn is_rust_module_root(source_file: &std::path::Path) -> bool {
    if matches!(
        source_file.file_stem().and_then(|s| s.to_str()),
        Some("mod" | "lib" | "main")
    ) {
        return true;
    }
    let Some(dir) = source_file.parent() else {
        return false;
    };
    let dir_name = dir.file_name().and_then(|n| n.to_str());
    let in_src = |d: &std::path::Path| d.components().any(|c| c.as_os_str() == "src");
    let is_build_rs = source_file.file_name().is_some_and(|n| n == "build.rs");
    match dir_name {
        Some("bin") => dir
            .parent()
            .and_then(|g| g.file_name())
            .is_some_and(|n| n == "src"),
        Some("examples" | "benches" | "tests") => !in_src(dir),
        _ => is_build_rs && !in_src(dir),
    }
}

/// Directory that holds the child modules of `source_file`'s module — the
/// base of `self::`. Rust 2018 puts the children of `a/b.rs` in `a/b/`.
pub(crate) fn rust_module_dir(source_file: &std::path::Path) -> Option<std::path::PathBuf> {
    let own_dir = source_file.parent()?;
    if is_rust_module_root(source_file) {
        Some(own_dir.to_path_buf())
    } else {
        Some(own_dir.join(source_file.file_stem()?))
    }
}

/// Expand a Rust `use`-path module specifier to the caller crate's
/// `src/<segments>` base so Tier-2 import resolution can pin the declaring
/// module. Returns `None` for non-Rust specifiers (TS/Python/etc. keep their
/// existing relative-resolution branches).
///
/// Handles the workspace-internal forms only — external crates (`std::`,
/// `serde::`) are never indexed, so their expanded base never matches a
/// SymbolTable key and resolution correctly falls through:
/// * `crate::output` from `crates/ecp-cli/src/commands/find.rs`
///   → `crates/ecp-cli/src/output`; from a repo-root crate's
///   `src/commands/find.rs` → `src/output`
/// * `self::a` → `a` under the caller module's child directory
///   (`a/b.rs` → `a/b/a`, `a/b/mod.rs` → `a/b/a`)
/// * `super::a` → `a` under the parent module's directory
///   (`a/b.rs` → `a/a`, `a/b/mod.rs` → `a/a`)
///
/// The trailing item name is NOT part of `import.source` (the parser splits
/// `use crate::output::{emit}` into source=`crate::output`, name=`emit`), so
/// every `::` segment here is a module path component.
fn rust_module_path_base(
    source_file: &std::path::Path,
    specifier: &str,
) -> Option<std::path::PathBuf> {
    if !source_file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
    {
        return None;
    }
    let segs: Vec<&str> = specifier.split("::").filter(|s| !s.is_empty()).collect();
    let (anchor, rest) = match segs.split_first()? {
        (&"crate", rest) => {
            let path = source_file.to_string_lossy().replace('\\', "/");
            let src_root = match path.rsplit_once("/src/") {
                Some((root, _)) => format!("{root}/src"),
                // A crate at the repo root: its repo-relative paths start at
                // `src/`, with no `/src/` segment to split on.
                None if path.starts_with("src/") => "src".to_owned(),
                None => return None,
            };
            (std::path::PathBuf::from(src_root), rest)
        }
        (&"self", rest) => (rust_module_dir(source_file)?, rest),
        (&"super", mut rest) => {
            // `super` is the parent of the file's own module, so it is
            // `rust_module_dir`'s parent; each further `super` climbs one
            // more module.
            let mut anchor = rust_module_dir(source_file)?.parent()?.to_path_buf();
            while let Some((&"super", tail)) = rest.split_first() {
                anchor = anchor.parent()?.to_path_buf();
                rest = tail;
            }
            (anchor, rest)
        }
        _ => return None,
    };
    Some(rest.iter().fold(anchor, |p, seg| p.join(seg)))
}

fn split_qualifier(name: &str) -> Option<(&str, &str)> {
    // Pick the rightmost separator. PHP's `\` is included alongside `::`
    // and `.` so `\App\helper` resolves with `App` as qualifier. `::` is
    // 2-char (preferred on tie since the position points at the first `:`,
    // not the `.`/`\` that might appear at the same column).
    let candidates = [
        (name.rfind("::"), 2usize),
        (name.rfind('.'), 1),
        (name.rfind('\\'), 1),
    ];
    let (split_idx, sep_len) = candidates
        .iter()
        .filter_map(|&(idx, len)| idx.map(|i| (i, len)))
        .max_by_key(|&(i, _)| i)?;
    let (before, after) = name.split_at(split_idx);
    let member = &after[sep_len..];
    if before.is_empty() || member.is_empty() {
        return None;
    }
    let qualifier = before
        .rsplit_once("::")
        .or_else(|| before.rsplit_once('.'))
        .or_else(|| before.rsplit_once('\\'))
        .map(|(_, q)| q)
        .unwrap_or(before);
    if qualifier.is_empty() {
        return None;
    }
    Some((qualifier, member))
}

/// Extensions probed during L0 candidate enumeration (covers every
/// language whose parser is wired into ecp-analyzer).
const EXT_CANDIDATES: &[&str] = &[
    ".ts", ".tsx", ".jsx", ".js", ".mjs", ".cjs", ".py", ".pyi", ".rs", ".go", ".java", ".kt",
    ".rb", ".php", ".cs", ".swift", ".dart", ".sol", ".sql",
];

/// Package-style suffixes — a directory acting as a module.
const INDEX_SUFFIXES: &[&str] = &[
    "/index.ts",
    "/index.tsx",
    "/index.js",
    "/index.jsx",
    "/__init__.py",
    "/mod.rs",
    "/lib.rs",
    "/main.rs",
];

/// L0 path normalization: enumerate every `SymbolTable` file key that
/// `specifier` could plausibly map to, invoking `visit` for each. The
/// closure returns `true` to keep going, `false` to short-circuit.
///
/// * **Verbatim specifier** is visited first so behavior is a strict
///   superset of pre-L0.
/// * **Relative** (`./x`, `../x`, `.x`, `..x.y`): joined against the
///   source file's parent directory, accounting for Python-style
///   multi-dot prefixes and dotted submodule paths (`from .a.b import C`).
/// * **Both relative and absolute**: try common extensions (`.ts .tsx .py
///   .rs ...`) and package-style suffixes (`/index.ts`, `/__init__.py`,
///   `/mod.rs`).
///
/// A single `String` buffer is reused across all suffixed probes, so
/// total allocations per call are bounded by O(1) heap activity once
/// the closure starts running. This matters on the resolver hot path
/// where Tier 2 fires once per (callsite, heritage, type, framework-ref).
fn for_each_specifier_candidate<F>(
    source_file: &std::path::Path,
    specifier: &str,
    aliases: &PathAliases,
    mut visit: F,
) where
    F: FnMut(&str) -> bool,
{
    if !visit(specifier) {
        return;
    }

    // Alias expansion (TS `tsconfig.json` paths, etc.) runs *before*
    // relative resolution: aliased specifiers like `@/utils` never look
    // like a relative path and would otherwise fall straight through to
    // the Tier-3 global fallback. Each expansion goes through the same
    // extension/index suffix probing as the relative branch.
    if !aliases.is_empty() {
        let mut stopped = false;
        aliases.expand(specifier, |expanded| {
            if probe_with_suffixes(expanded, &mut visit) {
                true
            } else {
                stopped = true;
                false
            }
        });
        if stopped {
            return;
        }
    }

    let dir = source_file.parent().unwrap_or(std::path::Path::new(""));
    let mut rust_module = false;
    let base_path: Option<std::path::PathBuf> = if let Some(rest) = specifier.strip_prefix("./") {
        Some(dir.join(rest))
    } else if specifier.starts_with("../") {
        let mut p = dir.to_path_buf();
        let mut s = specifier;
        while let Some(rest) = s.strip_prefix("../") {
            p = p.parent().unwrap_or(std::path::Path::new("")).to_path_buf();
            s = rest;
        }
        Some(p.join(s))
    } else if let Some(rust_base) = rust_module_path_base(source_file, specifier) {
        // Rust `use` path: `crate::output`, `super::a::b`, `self::x`, or a
        // workspace-internal `<crate>::mod::item` head. Map to the caller
        // crate's `src/<segments>` so Tier-2 import resolution can pin a
        // bare `emit()` call to its declaring module instead of dropping to
        // the Tier-3 same-name ambiguity cap. Without this, `ecp impact`
        // undercounts callers of any common-named cross-module Rust fn
        // (the #100-122 incident: `emit` reported 1 of 21 callers).
        rust_module = true;
        Some(rust_base)
    } else if specifier.starts_with('.') {
        // Python-style relative: count leading dots, then a dotted submodule
        // path. `.foo` from `src/pkg/x.py` → `src/pkg/foo`. `..foo.bar` →
        // walk parent once, then `foo/bar`. `...foo` → walk two parents,
        // then `foo` (PEP 328: N dots = walk N-1 packages).
        let dots = specifier.bytes().take_while(|&b| b == b'.').count();
        let rest = &specifier[dots..];
        // Strip any leftover leading `.` (e.g. `....foo` past the dot count)
        // and the implicit leading `/` that Path::join would otherwise treat
        // as absolute and discard the base.
        let dotted = rest.trim_start_matches('.').replace('.', "/");
        let dotted = dotted.trim_start_matches('/');
        let mut p = dir.to_path_buf();
        for _ in 1..dots {
            p = p.parent().unwrap_or(std::path::Path::new("")).to_path_buf();
        }
        Some(if dotted.is_empty() { p } else { p.join(dotted) })
    } else {
        None
    };

    let base = if let Some(b) = base_path {
        let b_str = b.to_string_lossy().replace('\\', "/");
        Some(
            b_str
                .trim_start_matches("./")
                .trim_end_matches('/')
                .to_string(),
        )
    } else if !specifier.contains("://") && !specifier.is_empty() {
        // Absolute-but-pathlike: `a/b` style. Still worth probing.
        Some(specifier.trim_end_matches('/').to_string())
    } else {
        None
    };

    let Some(base) = base else { return };

    if !rust_module {
        probe_with_suffixes(&base, &mut visit);
    } else if probe_rust_module(&base, &mut visit) {
        if let Some(fallback) = rust_self_fallback_base(source_file, specifier) {
            let fallback = fallback.to_string_lossy().replace('\\', "/");
            probe_rust_module(fallback.trim_start_matches("./"), &mut visit);
        }
    }
}

/// [`probe_with_suffixes`] for a Rust module path, which only a Rust file
/// can declare: `base.rs`, then the directory module files. A same-stem
/// `base.ts` beside the module is another language's file, not this module.
/// `lib.rs` / `main.rs` cover `crate` itself (`use crate::X` maps to `src`).
fn probe_rust_module<F>(base: &str, visit: &mut F) -> bool
where
    F: FnMut(&str) -> bool,
{
    [".rs", "/mod.rs", "/lib.rs", "/main.rs"]
        .iter()
        .all(|suffix| visit(&format!("{base}{suffix}")))
}

/// `self::rest` from an ordinary module file, anchored at the file's own
/// directory: where a crate root keeps its children. A root named by a
/// Cargo `[lib] path` / `[[bin]] path` (e.g. `src/api.rs`) is not
/// recognisable from its file name, so it is probed only after the module's
/// own child directory came up empty.
fn rust_self_fallback_base(
    source_file: &std::path::Path,
    specifier: &str,
) -> Option<std::path::PathBuf> {
    let rest = specifier.strip_prefix("self::")?;
    let is_rs = source_file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("rs"));
    if !is_rs || is_rust_module_root(source_file) {
        return None;
    }
    let dir = source_file.parent()?;
    Some(
        rest.split("::")
            .filter(|s| !s.is_empty())
            .fold(dir.to_path_buf(), |p, seg| p.join(seg)),
    )
}

/// Probe `base`, then `base + ext` for each known extension, then
/// `base + index_suffix` for each known index suffix. Returns `false`
/// if the visitor short-circuited, `true` if all probes were exhausted
/// without finding a hit. Factored out of `for_each_specifier_candidate`
/// so the alias-expansion branch reuses the same probing pattern.
fn probe_with_suffixes<F>(base: &str, visit: &mut F) -> bool
where
    F: FnMut(&str) -> bool,
{
    if !visit(base) {
        return false;
    }
    let mut buf = String::with_capacity(base.len() + 16);
    for ext in EXT_CANDIDATES {
        buf.clear();
        buf.push_str(base);
        buf.push_str(ext);
        if !visit(&buf) {
            return false;
        }
    }
    for suf in INDEX_SUFFIXES {
        buf.clear();
        buf.push_str(base);
        buf.push_str(suf);
        if !visit(&buf) {
            return false;
        }
    }
    true
}

/// Outcome of the receiver-typing ladder for one qualified callee.
enum TypeMember {
    Edge(NodeId, DecisionTier, f32),
    /// Several candidate types could own the member: no edge, and the dump
    /// records `AmbiguousGlobal`.
    Ambiguous,
    NoEdge,
}

/// The project types a qualifier can name (ladder step 1).
enum TypeCandidates<'q> {
    /// One type, with the name it is declared under (an import alias names
    /// the type differently from the qualifier).
    Unique(NodeId, &'q str),
    Set(Vec<NodeId>),
    /// An import binds the qualifier to a file outside the project.
    External,
    None,
}

/// Whether a type owns a member, directly or through its supertypes.
enum Ownership {
    Owned {
        id: NodeId,
        inherited: bool,
    },
    NotOwned,
    /// The member may come from a base the index cannot see, or from one of
    /// several owners: no edge.
    Unknown,
}

/// The owner index's answer for one type and one member name.
enum MemberLookup {
    Hit(NodeId),
    Miss,
    Ambiguous,
}

impl<'a> Resolver<'a> {
    /// Resolve `qualifier` as a Type (Class / Interface) via Tier 1 → Tier 2 →
    /// Tier 3 (kind-filtered, unique-only), returning the file_path of the
    /// resolved target. Used by Tier 2.5 to scope member lookup to the
    /// qualifier's defining file. Telemetry-silent — internal recursion is
    /// not surfaced in the decision dump.
    fn resolve_qualifier_file(
        &self,
        source_file: &Path,
        qualifier: &str,
        member: &str,
        target: ResolveTarget,
        raw_imports: &[RawImport],
        full_callee: Option<&str>,
    ) -> Option<String> {
        #[cfg(not(windows))]
        let source_file_str = source_file.to_string_lossy();
        #[cfg(windows)]
        let source_file_str =
            std::borrow::Cow::Owned(source_file.to_string_lossy().replace('\\', "/"));

        // Tier 1: same-file qualifier definition. Qualifiers are class /
        // interface / namespace / module names, so filter to Qualifier
        // here — avoids a property named `Logger` winning over `class
        // Logger` in the same file, while still letting `namespace outer
        // { ... }` and inline `mod foo { ... }` serve as the leading
        // segment of `outer::member()` / `foo::member()` calls.
        //
        // Member-presence gate: file-backed `mod auto_ensure;` declares
        // the Module node in the parent file, but the actual members
        // live in `auto_ensure.rs`. Returning the parent file here would
        // short-circuit the downstream member lookup AND block Tier 4's
        // module-file fallback. Only claim Tier 1 when the full chain
        // (qualifier → file → member) resolves in the same file —
        // matches the inline-mod / inline-namespace case and declines
        // for the file-backed stub case, letting later tiers handle it.
        if let Some(id) = self.symbol_table.lookup_in_file_with_kind(
            &source_file_str,
            qualifier,
            ResolveTarget::Qualifier,
        ) {
            if let Some(qf) = self.symbol_table.file_of(id) {
                if self
                    .symbol_table
                    .lookup_in_file_with_kind(qf, member, target)
                    .is_some()
                {
                    return Some(qf.to_string());
                }
            }
        }

        // Tier 2: imported qualifier (matches alias or imported_name; expands
        // specifier via the same L0 candidate enumeration used by the bare-
        // name resolver). Same member-presence gate as Tier 1: the import
        // tells us which file the qualifier lives in, but that file must
        // also contain the member to claim this tier.
        for import in raw_imports {
            let matches_qualifier = match &import.alias {
                Some(alias) => alias == qualifier,
                None => import.imported_name == qualifier,
            };
            if !matches_qualifier
                || full_callee.is_some_and(|callee| {
                    !rust_path_prefix_agrees_with_import(
                        callee,
                        qualifier,
                        &import.source,
                        raw_imports,
                    )
                })
            {
                continue;
            }
            let exported = &import.imported_name;
            let mut hit: Option<String> = None;
            for_each_specifier_candidate(
                source_file,
                &import.source,
                &self.path_aliases,
                |candidate| {
                    let qualifier_present = self
                        .symbol_table
                        .lookup_in_file_with_kind(candidate, exported, ResolveTarget::Qualifier)
                        .is_some();
                    let member_present = self
                        .symbol_table
                        .lookup_in_file_with_kind(candidate, member, target)
                        .is_some();
                    if qualifier_present && member_present {
                        hit = Some(candidate.to_string());
                        false
                    } else {
                        true
                    }
                },
            );
            if hit.is_some() {
                return hit;
            }
        }

        // Tier 3: kind-filtered unique global. Language + vendor barriers
        // applied via FileMeta — the same defences as bare-name Tier 3.
        // Member-presence gate identical to Tier 1's: blocks Module's
        // declaration-file from outranking the file-stem fallback when
        // members live elsewhere (`mod foo;` declaration in lib.rs vs.
        // `fn bar()` body in foo.rs).
        let caller_meta = FileMeta::from_path(&source_file_str);
        if let GlobalPick::Unique(id) =
            self.symbol_table
                .lookup_global(qualifier, ResolveTarget::Qualifier, caller_meta)
        {
            if let Some(qf) = self.symbol_table.file_of(id) {
                if self
                    .symbol_table
                    .lookup_in_file_with_kind(qf, member, target)
                    .is_some()
                {
                    return Some(qf.to_string());
                }
            }
        }

        // Tier 4: module-file fallback. The qualifier didn't match any Type
        // anywhere, but Rust / Python / similar languages let a *module name*
        // act as a qualifier (`mod auto_ensure;` ↔ `auto_ensure.rs`, `import
        // foo` ↔ `foo.py`). Walk the registered file paths and look for one
        // whose stem matches the qualifier and lives in the caller's crate.
        //
        // Fires only from Tier 2.5 (qualified-call resolution) — heritage
        // resolution passes `None` because parent class names should resolve
        // as Types, not as module files.
        //
        // Two false-positive defences:
        //
        //   1. **Internal-prefix check** — the qualifier's preceding segments
        //      must be empty (bare relative path), or be `crate`/`self`/`super`.
        //      `std::fs::read` becomes ("fs", "read") after split_qualifier;
        //      its preceding segment is `std` → external → declined.
        //
        //   2. **Same-crate prefix** — caller and candidate must share the
        //      same `*/src/` (or `*/tests/`) ancestor. Defends single-crate
        //      repos where the internal-prefix check alone would let a
        //      `std::fs::read` call bind to a workspace `src/fs.rs` — the
        //      preceding segment is `std` so check (1) handles that; check
        //      (2) is the secondary defence if a future extern crate name
        //      manages to slip past check (1).
        let full = full_callee?;
        if !qualifier_prefix_is_internal(full, qualifier) {
            return None;
        }
        let caller_prefix = crate_root_prefix(&source_file_str);
        let mut hit: Option<&str> = None;
        for fp in self.symbol_table.files_by_stem(qualifier) {
            if crate_root_prefix(fp) != caller_prefix {
                continue;
            }
            if hit.is_some() {
                return None;
            }
            hit = Some(fp);
        }
        hit.map(str::to_string)
    }

    /// Receiver-typing ladder (after Tier 3.5): resolve `qualifier.member`
    /// through the member's owning type instead of the qualifier's file.
    ///
    /// It emits an edge only when the qualifier names a project type, and
    /// that type or one of its resolved declared supertypes owns the member,
    /// and no unresolved base can shadow the owner. It never matches the
    /// member name alone.
    #[allow(clippy::too_many_arguments)]
    fn resolve_member_via_type(
        &self,
        source_file: &Path,
        source_file_str: &str,
        symbol_name: &str,
        qualifier: &str,
        member: &str,
        target: ResolveTarget,
        raw_imports: &[RawImport],
    ) -> TypeMember {
        // Callable only: the supertypes pre-pass resolves heritage with this
        // resolver before the supertypes exist, so a Type-target ladder would
        // make Pass 2 heritage edges disagree with the pre-pass. A path whose
        // prefix is an external module (`std::sync::Arc::new`) names a type
        // the project cannot own, the same guard Tier 4 applies.
        if target != ResolveTarget::Callable
            || !qualifier_prefix_is_internal(symbol_name, qualifier)
        {
            return TypeMember::NoEdge;
        }
        let confidence = |tier: ResolutionTier| tier.base_confidence();
        match self.type_candidates(source_file, source_file_str, qualifier, raw_imports) {
            TypeCandidates::Unique(ty, ty_name) => {
                match self.member_ownership(ty, ty_name, member, target) {
                    Ownership::Owned {
                        id,
                        inherited: false,
                    } => TypeMember::Edge(
                        id,
                        DecisionTier::TypeOwned,
                        confidence(ResolutionTier::QualifierScoped),
                    ),
                    Ownership::Owned {
                        id,
                        inherited: true,
                    } => TypeMember::Edge(
                        id,
                        DecisionTier::TypeHeritage,
                        confidence(ResolutionTier::HeritageScoped),
                    ),
                    Ownership::NotOwned | Ownership::Unknown => TypeMember::NoEdge,
                }
            }
            TypeCandidates::Set(types) => {
                let mut owner_member: Option<NodeId> = None;
                for ty in types {
                    match self.member_ownership(ty, qualifier, member, target) {
                        Ownership::Owned { id, .. } if owner_member.is_none_or(|m| m == id) => {
                            owner_member = Some(id);
                        }
                        Ownership::NotOwned => {}
                        Ownership::Owned { .. } | Ownership::Unknown => {
                            return TypeMember::Ambiguous;
                        }
                    }
                }
                owner_member.map_or(TypeMember::NoEdge, |id| {
                    TypeMember::Edge(
                        id,
                        DecisionTier::TypeCandidates,
                        confidence(ResolutionTier::Global),
                    )
                })
            }
            TypeCandidates::External | TypeCandidates::None => TypeMember::NoEdge,
        }
    }

    /// Ladder step 1: the project types `qualifier` can name, with no member
    /// gate. Kind filter `is_type` only: a Module / Namespace qualifier keeps
    /// the Tier 2.5 / Tier 4 path.
    fn type_candidates<'q>(
        &self,
        source_file: &Path,
        source_file_str: &str,
        qualifier: &'q str,
        raw_imports: &'q [RawImport],
    ) -> TypeCandidates<'q> {
        let st = self.symbol_table;
        if let Some(id) =
            st.lookup_in_file_with_kind(source_file_str, qualifier, ResolveTarget::Type)
        {
            return TypeCandidates::Unique(id, qualifier);
        }
        for import in raw_imports {
            if import
                .alias
                .as_deref()
                .unwrap_or(import.imported_name.as_str())
                != qualifier
            {
                continue;
            }
            let exported = import.imported_name.as_str();
            let mut in_project = false;
            let mut hit: Option<NodeId> = None;
            for_each_specifier_candidate(source_file, &import.source, &self.path_aliases, |cand| {
                if !st.has_file(cand) {
                    return true;
                }
                in_project = true;
                hit = st.lookup_in_file_with_kind(cand, exported, ResolveTarget::Type);
                hit.is_none()
            });
            if let Some(id) = hit {
                return TypeCandidates::Unique(id, exported);
            }
            if !in_project {
                // A library type (`from psqlpy import Connection`): a project
                // type of the same name elsewhere is not this receiver.
                return TypeCandidates::External;
            }
        }
        let caller_meta = FileMeta::from_path(source_file_str);
        let ids = st.global_candidates(qualifier, ResolveTarget::Type, caller_meta);
        match ids.len() {
            0 => TypeCandidates::None,
            1 => TypeCandidates::Unique(ids[0], qualifier),
            _ => TypeCandidates::Set(ids),
        }
    }

    /// Ladder steps 2 and 3: does type `ty` own `member`, directly or through
    /// its resolved declared supertypes?
    ///
    /// The walk visits every ancestor (breadth-first, declared order, depth
    /// [`MAX_HERITAGE_DEPTH`]). The owner is the most derived owning type;
    /// two owners on separate branches depend on the language's method
    /// resolution order, so they give `Unknown`. An unresolved base visited
    /// before the owner (or anywhere, when no ancestor owns the member) may
    /// supply the member itself, so it also gives `Unknown`.
    fn member_ownership(
        &self,
        ty: NodeId,
        ty_name: &str,
        member: &str,
        target: ResolveTarget,
    ) -> Ownership {
        match self.owned_member(ty, ty_name, member, target) {
            MemberLookup::Hit(id) => {
                return Ownership::Owned {
                    id,
                    inherited: false,
                }
            }
            MemberLookup::Ambiguous => return Ownership::Unknown,
            MemberLookup::Miss => {}
        }
        let st = self.symbol_table;
        let mut seen: FxHashSet<NodeId> = FxHashSet::default();
        seen.insert(ty);
        let mut frontier = vec![ty];
        let mut unresolved_seen = false;
        // (owning ancestor, member id, an unresolved base came first)
        let mut hits: Vec<(NodeId, NodeId, bool)> = Vec::new();
        for _ in 0..MAX_HERITAGE_DEPTH {
            let mut next = Vec::new();
            for &current in &frontier {
                let Some(sup) = st.supertypes(current) else {
                    continue;
                };
                for (pos, &base) in sup.bases.iter().enumerate() {
                    unresolved_seen |= sup.first_unresolved == Some(pos as u32);
                    if !seen.insert(base) {
                        continue;
                    }
                    next.push(base);
                    let Some(base_name) = st.name_in_file(base) else {
                        continue;
                    };
                    match self.owned_member(base, base_name, member, target) {
                        MemberLookup::Hit(id) => hits.push((base, id, unresolved_seen)),
                        MemberLookup::Ambiguous => return Ownership::Unknown,
                        MemberLookup::Miss => {}
                    }
                }
                unresolved_seen |= sup.first_unresolved == Some(sup.bases.len() as u32);
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        // The depth cap cut the walk short: a deeper base may own the member.
        if frontier.iter().any(|&t| st.supertypes(t).is_some()) {
            return Ownership::Unknown;
        }
        let most_derived = hits.iter().find(|&&(owner, ..)| {
            let above = st.ancestors(owner);
            hits.iter()
                .all(|&(other, ..)| other == owner || above.contains(&other))
        });
        match most_derived {
            Some(&(_, id, false)) => Ownership::Owned {
                id,
                inherited: true,
            },
            Some(_) => Ownership::Unknown,
            None if !hits.is_empty() || unresolved_seen => Ownership::Unknown,
            None => Ownership::NotOwned,
        }
    }

    /// The member of `ty` named `member`, by the owner index: first in the
    /// type's own file, then in the type's crate / package scope.
    fn owned_member(
        &self,
        ty: NodeId,
        ty_name: &str,
        member: &str,
        target: ResolveTarget,
    ) -> MemberLookup {
        let st = self.symbol_table;
        let Some(ty_file) = st.file_of(ty) else {
            return MemberLookup::Miss;
        };
        let declares_owner =
            |file: &str| st.count_in_file_with_kind(file, ty_name, ResolveTarget::Qualifier);
        let in_file = st.owned_in_file(ty_file, member, ty_name, target);
        if let Some(&first) = in_file.first() {
            // Several hits are overloads of the one type, unless the file
            // declares two owners of that name (nested types, a type and a
            // module); the owner index cannot tell those apart.
            return if in_file.len() == 1 || declares_owner(ty_file) == 1 {
                MemberLookup::Hit(first)
            } else {
                MemberLookup::Ambiguous
            };
        }
        let elsewhere = st.owned_global(member, ty_name, target, ty_file);
        let Some(&first) = elsewhere.first() else {
            return MemberLookup::Miss;
        };
        if self.type_unique_in_scope(ty_name, ty_file) {
            return MemberLookup::Hit(first);
        }
        // Another same-named type shares the scope. A member in a file that
        // declares one of them belongs to that type; any other member cannot
        // be attributed.
        if elsewhere
            .iter()
            .all(|&id| st.file_of(id).is_some_and(|f| declares_owner(f) > 0))
        {
            MemberLookup::Miss
        } else {
            MemberLookup::Ambiguous
        }
    }

    /// True when `ty_file`'s type `ty_name` is the only qualifier of that
    /// name in the scope [`SymbolTable::owned_global`] searches: the same
    /// language and vendor barrier and crate root, and for Go one package.
    fn type_unique_in_scope(&self, ty_name: &str, ty_file: &str) -> bool {
        let st = self.symbol_table;
        let meta = FileMeta::from_path(ty_file);
        let root = crate_root_prefix(ty_file);
        let dir = Path::new(ty_file).parent();
        let same_package_only = meta.language == Language::Go;
        st.global_candidates(ty_name, ResolveTarget::Qualifier, meta)
            .into_iter()
            .filter(|&id| {
                st.file_of(id).is_some_and(|f| {
                    crate_root_prefix(f) == root
                        && (!same_package_only || Path::new(f).parent() == dir)
                })
            })
            .take(2)
            .count()
            == 1
    }

    /// Tier 3.5: attempt module-tree FQN resolution for Rust qualified calls.
    ///
    /// `symbol_name` is the full callee string (e.g.
    /// `"crate::build::orchestrator::build_l2"`).
    /// `member` is its last `::` segment (the item name, same value that
    /// `split_qualifier` already extracted for Tier 2.5).
    ///
    /// Returns the node id of the resolved target, or `None` if the module
    /// tree is absent, the FQN doesn't resolve, or the member isn't found in
    /// the resolved file.
    fn try_module_tree_resolve(
        &self,
        source_file_str: &str,
        symbol_name: &str,
        member: &str,
        target: ResolveTarget,
    ) -> Option<NodeId> {
        let tree = self.mod_tree?;
        let workspace_root = self.workspace_root.as_ref()?;
        let resolved = tree.resolve_fqn(symbol_name, source_file_str, workspace_root)?;
        self.symbol_table
            .lookup_in_file_with_kind(&resolved.file, &resolved.item_name, target)
            .or_else(|| {
                let bare = member.split('<').next().unwrap_or(member);
                self.symbol_table
                    .lookup_in_file_with_kind(&resolved.file, bare, target)
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        src_file: &str,
        name: &str,
        specifier: Option<&str>,
        tier: DecisionTier,
        target_id: Option<NodeId>,
        alt_count: u32,
        confidence: Option<f32>,
    ) {
        // Production path: `self.decisions` is `None` → single
        // Option-discriminant branch and we're out. No Mutex touch.
        let Some(cell) = self.decisions.as_ref() else {
            return;
        };
        let mut guard = cell.lock().expect("resolver dump mutex poisoned");
        guard.push(ResolverDecision {
            src_file: src_file.to_string(),
            name: name.to_string(),
            specifier: specifier.map(|s| s.to_string()),
            tier,
            target_id,
            alt_count,
            confidence,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cands(src: &str, spec: &str) -> Vec<String> {
        let mut out = Vec::new();
        let aliases = PathAliases::new();
        for_each_specifier_candidate(&PathBuf::from(src), spec, &aliases, |c| {
            out.push(c.to_string());
            true
        });
        out
    }

    #[test]
    fn verbatim_specifier_is_always_first_candidate() {
        let c = cands("src/a/b.ts", "./foo");
        assert_eq!(c[0], "./foo", "verbatim must lead the candidate list");
    }

    #[test]
    fn ts_dot_relative_resolves_against_source_dir_with_ext_and_index() {
        let c = cands("src/a/b.ts", "./foo");
        assert!(
            c.contains(&"src/a/foo.ts".to_string()),
            "should include src/a/foo.ts: {c:?}"
        );
        assert!(
            c.contains(&"src/a/foo/index.ts".to_string()),
            "should include src/a/foo/index.ts: {c:?}"
        );
    }

    #[test]
    fn ts_parent_relative_walks_up_one_dir() {
        let c = cands("src/a/b.ts", "../helpers/util");
        assert!(
            c.contains(&"src/helpers/util.ts".to_string()),
            "should include src/helpers/util.ts: {c:?}"
        );
    }

    #[test]
    fn python_single_dot_resolves_to_current_package() {
        let c = cands("src/flask/__init__.py", ".globals");
        assert!(
            c.contains(&"src/flask/globals.py".to_string()),
            "should include src/flask/globals.py: {c:?}"
        );
    }

    #[test]
    fn python_dotted_submodule_replaces_dots_with_slashes() {
        let c = cands("src/pkg/x.py", ".sub.mod");
        assert!(
            c.contains(&"src/pkg/sub/mod.py".to_string()),
            "should include src/pkg/sub/mod.py: {c:?}"
        );
    }

    #[test]
    fn python_double_dot_walks_one_parent_then_drills() {
        let c = cands("src/pkg/inner/x.py", "..helpers.util");
        assert!(
            c.contains(&"src/pkg/helpers/util.py".to_string()),
            "should include src/pkg/helpers/util.py: {c:?}"
        );
    }

    /// Regression: `...foo` was generating `/foo` because `rest = "/foo"`
    /// would slip through to `Path::join("/foo")`, which on Unix discards
    /// the base. PEP 328: three dots = walk two parents.
    #[test]
    fn python_triple_dot_walks_two_parents() {
        let c = cands("src/a/b/c/d.py", "...mod");
        assert!(
            c.contains(&"src/a/mod.py".to_string()),
            "should include src/a/mod.py (walked two parents): {c:?}"
        );
        // No /-rooted entries from the dotted base — would mean the bug
        // came back.
        assert!(
            !c.iter().any(|s| s.starts_with("/mod") || s == "/"),
            "no absolute-rooted candidate should leak: {c:?}"
        );
    }

    #[test]
    fn bare_pathlike_specifier_still_emits_extension_probes() {
        let c = cands("any.ts", "components/Button");
        assert!(
            c.contains(&"components/Button.tsx".to_string())
                || c.contains(&"components/Button.ts".to_string()),
            "bare specifier should still trigger ext probes: {c:?}"
        );
    }

    #[test]
    fn package_style_index_suffix_is_offered() {
        let c = cands("src/a/b.ts", "./foo");
        assert!(
            c.iter().any(|s| s.ends_with("/index.tsx")),
            "should include some /index.tsx candidate: {c:?}"
        );
    }

    // ── Tier-3 cap (kind-filtered + unique-only) ────────────────────────────

    use ecp_core::graph::NodeKind;

    /// Build a SymbolTable from `(file, name, kind)` triples — ids auto-assigned
    /// monotonically (matching the dense-id invariant `register_node` enforces).
    fn st_with(nodes: &[(&str, &str, NodeKind)]) -> SymbolTable {
        let mut st = SymbolTable::new();
        for (id, (file, name, kind)) in nodes.iter().enumerate() {
            st.register_node(file, name, id as u32, *kind);
        }
        st
    }

    #[test]
    fn tier3_ambiguous_callable_emits_no_edge() {
        // Two same-named methods in different files → ambiguous bare call
        // refuses to guess. Pins the dominant defence against fan-out
        // (common names like `new`/`format`/`default`/`main`).
        let st = st_with(&[
            ("a.rs", "new", NodeKind::Method),
            ("b.rs", "new", NodeKind::Method),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(&PathBuf::from("c.rs"), "new", &[], ResolveTarget::Callable);
        assert!(
            out.is_empty(),
            "ambiguous bare callable must not emit, got {:?}",
            out
        );
    }

    #[test]
    fn tier3_unique_callable_emits_one_edge() {
        // Single global match → still emit. The cap is about ambiguity,
        // not killing all cross-file resolution.
        let st = st_with(&[("a.rs", "process_request", NodeKind::Function)]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("c.rs"),
            "process_request",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(out, vec![(0, ResolutionTier::Global.base_confidence())]);
    }

    #[test]
    fn tier3_kind_filter_excludes_non_callable() {
        // One Function + one Variable share the name. Callable target sees
        // only the Function → uniqueness restored → edge emitted. Without
        // the kind filter, both would match → ambiguous → no edge.
        let st = st_with(&[
            ("a.rs", "config", NodeKind::Function),
            ("b.rs", "config", NodeKind::Variable),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("c.rs"),
            "config",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(out, vec![(0, ResolutionTier::Global.base_confidence())]);
    }

    #[test]
    fn tier1_same_file_kind_filters_out_non_callable() {
        // SameFile is now kind-aware: a Variable named `helper` in the same
        // file no longer "wins" for a Callable target — it would yield the
        // semantically-nonsense `Calls -> Variable` edge that PR #71 round-3
        // set out to remove. Falls through to Tier-3 Global, which picks
        // up the Function in b.rs (unique under the Callable predicate).
        let st = st_with(&[
            ("a.rs", "helper", NodeKind::Variable),
            ("b.rs", "helper", NodeKind::Function),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("a.rs"),
            "helper",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(out, vec![(1, ResolutionTier::Global.base_confidence())]);
    }

    // ── Layer-1 barriers (language + vendor) ────────────────────────────────

    #[test]
    fn tier3_language_barrier_blocks_cross_language() {
        // Rust caller's bare `is_some` must not resolve to a uniquely-named
        // Move function. Pins against the residual fan-out where a Rust
        // `result.is_some()` was wrongly connecting to a vendor `.move` test
        // fixture's `is_some` definition.
        let st = st_with(&[("lib/option.move", "is_some", NodeKind::Function)]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("src/caller.rs"),
            "is_some",
            &[],
            ResolveTarget::Callable,
        );
        assert!(
            out.is_empty(),
            "rust caller must not cross language boundary to move target, got {:?}",
            out
        );
    }

    #[test]
    fn tier3_vendor_barrier_blocks_source_caller() {
        // A unique callable defined under `/vendor/` is invisible to a
        // non-vendor caller. Pins vendor test corpora away from production
        // resolution surface even when language and uniqueness match.
        let st = st_with(&[(
            "crates/vendor/tree-sitter-x/tests/helper.rs",
            "uniquely_named_helper",
            NodeKind::Function,
        )]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("crates/ecp-cli/src/main.rs"),
            "uniquely_named_helper",
            &[],
            ResolveTarget::Callable,
        );
        assert!(
            out.is_empty(),
            "non-vendor caller must not reach vendor target, got {:?}",
            out
        );
    }

    #[test]
    fn tier3_intra_vendor_resolution_preserved() {
        // Vendor → vendor calls remain resolvable. The barrier is asymmetric
        // by design (source ↛ vendor, but vendor ↔ vendor is fine for the
        // vendor crate's internal cohesion).
        let st = st_with(&[(
            "crates/vendor/tree-sitter-x/src/helper.rs",
            "vendor_helper",
            NodeKind::Function,
        )]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("crates/vendor/tree-sitter-x/src/caller.rs"),
            "vendor_helper",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(
            out,
            vec![(0, ResolutionTier::Global.base_confidence())],
            "intra-vendor resolution must still emit, got {:?}",
            out
        );
    }

    // ── split_qualifier ─────────────────────────────────────────────────────

    #[test]
    fn split_qualifier_handles_simple_double_colon() {
        assert_eq!(split_qualifier("A::new"), Some(("A", "new")));
    }

    #[test]
    fn split_qualifier_takes_last_segment_for_multi_path() {
        // `std::vec::Vec::new` — Vec is the immediate qualifier; `std::vec`
        // is a path prefix that the symbol table can't map back to a single
        // registered Type name.
        assert_eq!(split_qualifier("std::vec::Vec::new"), Some(("Vec", "new")));
    }

    #[test]
    fn split_qualifier_handles_dot_separator() {
        assert_eq!(split_qualifier("obj.method"), Some(("obj", "method")));
    }

    #[test]
    fn split_qualifier_returns_none_for_bare_name() {
        assert_eq!(split_qualifier("foo"), None);
    }

    #[test]
    fn split_qualifier_rejects_empty_sides() {
        assert_eq!(split_qualifier("::foo"), None);
        assert_eq!(split_qualifier("foo::"), None);
        assert_eq!(split_qualifier(".foo"), None);
        assert_eq!(split_qualifier("foo."), None);
    }

    // ── Tier 2.5: qualifier-scoped resolution ───────────────────────────────

    #[test]
    fn tier2_5_resolves_via_same_file_qualifier() {
        // `A` defined in caller's file, `new` defined in A's file (`a.rs`).
        // Caller `c.rs` invokes `A::new` — Tier 2.5 should:
        //   1. Resolve `A` as Type via Tier 1/2/3 → finds A's file (a.rs)
        //   2. Lookup `new` in a.rs → finds it
        //   3. Emit edge at QualifierScoped confidence (0.85)
        let st = st_with(&[
            ("a.rs", "A", NodeKind::Class),
            ("a.rs", "new", NodeKind::Method),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("a.rs"),
            "A::new",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(
            out,
            vec![(1, ResolutionTier::QualifierScoped.base_confidence())]
        );
    }

    #[test]
    fn tier2_5_resolves_via_global_qualifier() {
        // `A` defined in `a.rs`, caller in different file. Qualifier resolves
        // via Tier 3 (kind-filtered, unique Type), member then found in A's
        // file. This is the dominant Rust pattern (`A::new()` from another
        // module).
        let st = st_with(&[
            ("a.rs", "A", NodeKind::Class),
            ("a.rs", "new", NodeKind::Method),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.rs"),
            "A::new",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(
            out,
            vec![(1, ResolutionTier::QualifierScoped.base_confidence())]
        );
    }

    #[test]
    fn tier2_5_unknown_qualifier_emits_nothing() {
        // No `A` registered as a Type anywhere. Tier 2.5 must NOT fall back
        // to bare-name `new` Tier-3: dogfood verification (B.1 dump,
        // 27.8k decisions) showed a ~52% false-positive rate on that
        // fallback path — `std::fs::read` resolving to a local `read()`,
        // `serde_json::json!` macro resolving to a local `json()`, etc.
        // The proper recovery for legitimate module-qualified free functions
        // (`registry::sanitize_branch`) is Phase B.3 (workspace-crate-aware
        // import resolution), not bare-name fallback.
        let st = st_with(&[("a.rs", "new", NodeKind::Method)]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.rs"),
            "A::new",
            &[],
            ResolveTarget::Callable,
        );
        assert!(out.is_empty(), "unknown qualifier must not emit: {:?}", out);
    }

    #[test]
    fn tier2_5_member_missing_in_qualifier_file_emits_nothing() {
        // Qualifier `A` resolves to `a.rs`, but `a.rs` doesn't define
        // `nonexistent`. Member missing → no edge (no Tier-3 fallback for
        // short name even though it might be unique globally elsewhere).
        let st = st_with(&[
            ("a.rs", "A", NodeKind::Class),
            ("b.rs", "nonexistent", NodeKind::Function),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.rs"),
            "A::nonexistent",
            &[],
            ResolveTarget::Callable,
        );
        assert!(
            out.is_empty(),
            "member missing in qualifier's file must not emit: {:?}",
            out
        );
    }

    #[test]
    fn tier2_5_ambiguous_qualifier_emits_nothing() {
        // Two Types named `A` in different files. The unique-only constraint
        // on the qualifier's Tier-3 step rejects ambiguity → no edge. Member
        // existing globally is irrelevant — qualified callees never degrade
        // to bare-name Tier 3 (see `tier2_5_unknown_qualifier_emits_nothing`
        // for the rationale).
        let st = st_with(&[
            ("a.rs", "A", NodeKind::Class),
            ("b.rs", "A", NodeKind::Class),
            ("a.rs", "new", NodeKind::Method),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.rs"),
            "A::new",
            &[],
            ResolveTarget::Callable,
        );
        assert!(
            out.is_empty(),
            "ambiguous qualifier must not emit: {:?}",
            out
        );
    }

    #[test]
    fn tier2_5_does_not_fall_back_to_tier3_for_qualified_callee() {
        // The member `unique_method` is globally unique AND would resolve via
        // bare-name Tier 3 if reached. But the callee is qualified
        // `Unknown::unique_method` and `Unknown` doesn't resolve → no edge.
        // Pins the no-guess policy: a qualified callee resolves via its
        // qualifier or not at all. Dump verification confirmed that allowing
        // this fallback restores pre-B.1 false positives (`std::fs::read` →
        // local `read()`, etc.) at a higher rate than it recovers legitimate
        // edges — those should come from Phase B.3 instead.
        let st = st_with(&[("a.rs", "unique_method", NodeKind::Function)]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.rs"),
            "Unknown::unique_method",
            &[],
            ResolveTarget::Callable,
        );
        assert!(
            out.is_empty(),
            "qualified callee with unresolved qualifier must not fall through to Tier-3: {:?}",
            out
        );
    }

    #[test]
    fn tier2_5_handles_multi_segment_qualifier_via_last_segment() {
        // `std::vec::Vec::new` — qualifier folds to last segment `Vec`,
        // which resolves uniquely to `vec.rs`, where `new` lives.
        let st = st_with(&[
            ("vec.rs", "Vec", NodeKind::Class),
            ("vec.rs", "new", NodeKind::Method),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.rs"),
            "std::vec::Vec::new",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(
            out,
            vec![(1, ResolutionTier::QualifierScoped.base_confidence())]
        );
    }

    #[test]
    fn tier2_5_resolves_via_import() {
        // TS-style: `import { MyClass } from "./x"` then `MyClass.foo()`.
        // Tier 2.5 should resolve the qualifier via the import (Tier 2) →
        // find foo in x.ts. Confirms the import path works for the qualifier
        // resolution sub-step, not just same-file / global.
        use ecp_core::analyzer::types::RawImport;
        let st = st_with(&[
            ("src/x.ts", "MyClass", NodeKind::Class),
            ("src/x.ts", "foo", NodeKind::Method),
        ]);
        let r = Resolver::new(&st);
        let imports = vec![RawImport {
            source: "./x".to_string(),
            imported_name: "MyClass".to_string(),
            alias: None,
            binding_kind: None,
        }];
        let out = r.resolve_symbol(
            &PathBuf::from("src/caller.ts"),
            "MyClass.foo",
            &imports,
            ResolveTarget::Callable,
        );
        assert_eq!(
            out,
            vec![(1, ResolutionTier::QualifierScoped.base_confidence())]
        );
    }

    #[test]
    fn tier2_rust_crate_path_import_disambiguates_ambiguous_callable() {
        // The #100-122 incident root cause: a Rust bare call `emit(...)` whose
        // `use crate::output::{emit}` import names the exact source module.
        // With several same-named `emit` definitions across the workspace, the
        // Tier-3 ambiguity cap suppresses the edge — UNLESS Tier 2 expands the
        // Rust module path `crate::output` to the caller crate's
        // `src/output.rs` and resolves there first. Without the crate:: path
        // expansion, `ecp impact` undercounts callers of any common-named
        // cross-module Rust function.
        use ecp_core::analyzer::types::RawImport;
        let st = st_with(&[
            ("crates/ecp-cli/src/output.rs", "emit", NodeKind::Function),
            (
                "crates/ecp-cli/src/commands/diff/output.rs",
                "emit",
                NodeKind::Function,
            ),
            (
                "crates/ecp-analyzer/src/javascript/path_literals.rs",
                "emit",
                NodeKind::Function,
            ),
        ]);
        let r = Resolver::new(&st);
        let imports = vec![RawImport {
            source: "crate::output".to_string(),
            imported_name: "emit".to_string(),
            alias: None,
            binding_kind: None,
        }];
        let out = r.resolve_symbol(
            &PathBuf::from("crates/ecp-cli/src/commands/find.rs"),
            "emit",
            &imports,
            ResolveTarget::Callable,
        );
        assert_eq!(
            out,
            vec![(0, ResolutionTier::ImportScoped.base_confidence())],
            "crate::output import must resolve `emit` to crates/ecp-cli/src/output.rs (node 0), \
             not be suppressed by the same-name ambiguity cap"
        );
    }

    #[test]
    fn rust_module_path_expansion_does_not_touch_non_rust_callers() {
        // Generality guard: the crate::/self::/super:: expansion is gated on a
        // `.rs` caller extension. A non-Rust caller (e.g. a Move file) with a
        // `crate::`-looking specifier must NOT be expanded — its own language's
        // resolution path stays authoritative, and a same-name ambiguity is
        // still correctly suppressed rather than mis-resolved.
        assert!(
            rust_module_path_base(&PathBuf::from("src/x.ts"), "crate::output").is_none(),
            "non-.rs caller must not trigger Rust module-path expansion"
        );
        assert!(
            rust_module_path_base(&PathBuf::from("lib/m.move"), "self::a").is_none(),
            "non-.rs caller must not trigger self:: expansion"
        );
        // External crate path from a Rust caller still expands (harmless): it
        // produces a base that no SymbolTable key matches, so resolution falls
        // through exactly as before — verified by it returning Some(path) here
        // but the std symbol never being indexed.
        assert!(
            rust_module_path_base(&PathBuf::from("crates/c/src/a.rs"), "std::fs").is_none(),
            "std:: (non crate/self/super head) must not expand"
        );
    }

    #[test]
    fn test_rust_module_path_base_super_from_non_mod_file_is_own_directory() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(base("a/b.rs", "super"), Some(PathBuf::from("a")));
        assert_eq!(
            base("src/commands/impact/symbol.rs", "super::x"),
            Some(PathBuf::from("src/commands/impact/x"))
        );
        assert_eq!(base("src/x.rs", "super"), Some(PathBuf::from("src")));
    }

    #[test]
    fn test_rust_module_path_base_super_from_mod_rs_is_parent_directory() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(base("a/b/mod.rs", "super"), Some(PathBuf::from("a")));
        assert_eq!(base("a/b/mod.rs", "super::x"), Some(PathBuf::from("a/x")));
    }

    #[test]
    fn test_rust_module_path_base_self_from_non_mod_file_is_file_stem_directory() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(base("a/b.rs", "self"), Some(PathBuf::from("a/b")));
        assert_eq!(base("a/b.rs", "self::x"), Some(PathBuf::from("a/b/x")));
    }

    #[test]
    fn test_rust_module_path_base_self_from_mod_rs_is_own_directory() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(base("a/b/mod.rs", "self"), Some(PathBuf::from("a/b")));
        assert_eq!(base("a/b/mod.rs", "self::x"), Some(PathBuf::from("a/b/x")));
    }

    #[test]
    fn test_rust_module_path_base_crate_roots_keep_parent_directory_semantics() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(
            base("c/src/lib.rs", "self::x"),
            Some(PathBuf::from("c/src/x"))
        );
        assert_eq!(
            base("c/src/main.rs", "self::x"),
            Some(PathBuf::from("c/src/x"))
        );
        assert_eq!(base("c/src/lib.rs", "super"), Some(PathBuf::from("c")));
        assert_eq!(base("c/src/main.rs", "super"), Some(PathBuf::from("c")));
        assert_eq!(
            base("c/src/a/b.rs", "crate::m"),
            Some(PathBuf::from("c/src/m"))
        );
    }

    #[test]
    fn test_rust_module_path_base_super_chain_walks_one_module_per_super() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        // a::b::c  ->  super = a::b (dir a/b), super::super = a (dir a)
        assert_eq!(base("a/b/c.rs", "super::super"), Some(PathBuf::from("a")));
        assert_eq!(
            base("a/b/c.rs", "super::super::x"),
            Some(PathBuf::from("a/x"))
        );
        // a::b (mod.rs)  ->  super = a, super::super = parent of a
        assert_eq!(
            base("r/a/b/mod.rs", "super::super"),
            Some(PathBuf::from("r"))
        );
    }

    #[test]
    fn test_rust_module_path_base_self_from_cargo_target_roots_is_own_directory() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(
            base("src/bin/tool.rs", "self"),
            Some(PathBuf::from("src/bin"))
        );
        assert_eq!(base("tests/it.rs", "self"), Some(PathBuf::from("tests")));
        assert_eq!(
            base("crates/foo/examples/demo.rs", "self"),
            Some(PathBuf::from("crates/foo/examples"))
        );
        assert_eq!(
            base("crates/foo/benches/b.rs", "self::x"),
            Some(PathBuf::from("crates/foo/benches/x"))
        );
        assert_eq!(base("build.rs", "self"), Some(PathBuf::from("")));
        assert_eq!(
            base("crates/foo/build.rs", "self::x"),
            Some(PathBuf::from("crates/foo/x"))
        );
    }

    #[test]
    fn test_rust_module_path_base_tests_dir_under_src_stays_ordinary_module() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(
            base("src/a/tests/x.rs", "self"),
            Some(PathBuf::from("src/a/tests/x"))
        );
        assert_eq!(
            base("src/a/tests/x.rs", "super"),
            Some(PathBuf::from("src/a/tests"))
        );
        assert_eq!(
            base("src/build.rs", "self"),
            Some(PathBuf::from("src/build"))
        );
        assert_eq!(base("bin/tool.rs", "self"), Some(PathBuf::from("bin/tool")));
    }

    #[test]
    fn test_rust_module_path_base_super_from_cargo_target_root_is_parent_directory() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(base("src/bin/tool.rs", "super"), Some(PathBuf::from("src")));
        assert_eq!(
            base("crates/foo/tests/it.rs", "super::x"),
            Some(PathBuf::from("crates/foo/x"))
        );
    }

    #[test]
    fn test_rust_module_path_base_crate_from_repo_root_crate_is_src() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(base("src/app.rs", "crate::a"), Some(PathBuf::from("src/a")));
        assert_eq!(
            base("src/lib.rs", "crate::a::b"),
            Some(PathBuf::from("src/a/b"))
        );
        assert_eq!(
            base("crates/x/src/app.rs", "crate::a"),
            Some(PathBuf::from("crates/x/src/a"))
        );
        assert_eq!(base("mysrc/app.rs", "crate::a"), None);
        assert_eq!(base("tests/it.rs", "crate::a"), None);
        assert_eq!(
            base("src\\app.rs", "crate::a"),
            Some(PathBuf::from("src/a"))
        );
        assert_eq!(
            base("crates\\x\\src\\app.rs", "crate::a"),
            Some(PathBuf::from("crates/x/src/a"))
        );
    }

    #[test]
    fn test_rust_path_prefix_agrees_with_import_cases() {
        let agrees = |callee: &str, q: &str, source: &str| {
            rust_path_prefix_agrees_with_import(callee, q, source, &[])
        };
        // No module path before the qualifier: nothing to disagree with.
        assert!(agrees("Error::custom", "Error", "crate::error"));
        assert!(agrees("custom", "Error", "crate::error"));
        assert!(agrees("err.custom", "err", "crate::error"));
        assert!(agrees("self::Error::custom", "Error", "crate::error"));
        assert!(agrees("::Error::custom", "Error", "crate::error"));
        // The named module path is a suffix of the import source.
        assert!(agrees(
            "crate::error::Error::custom",
            "Error",
            "crate::error"
        ));
        // A crate-rooted path names one module, not any module ending in it.
        assert!(!agrees(
            "crate::error::Error::custom",
            "Error",
            "crate::other::error"
        ));
        assert!(agrees("error::Error::custom", "Error", "crate::error"));
        assert!(agrees(
            "super::error::Error::custom",
            "Error",
            "crate::error"
        ));
        // Another module names another item.
        assert!(!agrees("de::Error::custom", "Error", "crate::error"));
        assert!(!agrees("crate::de::Error::custom", "Error", "crate::error"));
        assert!(!agrees("a::error::Error::custom", "Error", "crate::error"));
        // A module alias expands before the comparison, in both forms the
        // parser records.
        let alias = |source: &str, name: &str| RawImport {
            source: source.to_string(),
            imported_name: name.to_string(),
            alias: Some("err".to_string()),
            binding_kind: None,
        };
        for module in [
            alias("crate::error", "crate::error"),
            alias("crate", "error"),
        ] {
            let imports = [module];
            assert!(rust_path_prefix_agrees_with_import(
                "err::Error::custom",
                "Error",
                "crate::error",
                &imports
            ));
            assert!(!rust_path_prefix_agrees_with_import(
                "err::Error::custom",
                "Error",
                "crate::other",
                &imports
            ));
        }
        // The qualifier is not the segment before the member: not this rule.
        assert!(agrees("de::Error::custom", "de", "crate::error"));
    }

    #[test]
    fn test_rust_module_path_base_non_rust_file_with_super_is_untouched() {
        assert!(rust_module_path_base(&PathBuf::from("a/b.py"), "super::x").is_none());
        assert!(rust_module_path_base(&PathBuf::from("a/b.ts"), "self::x").is_none());
    }

    #[test]
    fn tier2_5_member_kind_filtered_inside_qualifier_file() {
        // PR #71 round-3 flipped Tier 2.5 to kind-aware lookup (the
        // previous "prefer recall" stance was producing `Calls -> Const`
        // and `Calls -> Variable` edges that have no operational meaning).
        // `A::FLAG` requesting Callable no longer resolves to the Const —
        // it falls through to Unresolved (Tier 3 Global filters on
        // Callable too, and no Callable named FLAG exists).
        let st = st_with(&[
            ("a.rs", "A", NodeKind::Class),
            ("a.rs", "FLAG", NodeKind::Const),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.rs"),
            "A::FLAG",
            &[],
            ResolveTarget::Callable,
        );
        assert!(
            out.is_empty(),
            "FLAG is a Const, must not surface as a Callable target; got {:?}",
            out
        );
    }

    // ── Ambiguity sentinel: distinguish "no candidates" from "many" ─────────

    #[test]
    fn tier3_ambiguous_records_ambiguous_global_decision_across_14_langs() {
        // For every mainstream language, two same-name global functions
        // produce zero edges (preserved behavior) AND a single
        // `AmbiguousGlobal` decision — replacing the prior `Unresolved`
        // outcome that conflated "not found" with "found ≥2, suppressed".
        let langs: &[&str] = &[
            "ts", "js", "py", "java", "kt", "cs", "go", "rs", "php", "rb", "swift", "c", "cpp",
            "dart",
        ];
        for ext in langs {
            let st = st_with(&[
                (
                    Box::leak(format!("a.{ext}").into_boxed_str()),
                    "ambiguity_demo",
                    NodeKind::Function,
                ),
                (
                    Box::leak(format!("b.{ext}").into_boxed_str()),
                    "ambiguity_demo",
                    NodeKind::Function,
                ),
            ]);
            let mut r = Resolver::new(&st);
            r.enable_dump();
            let out = r.resolve_symbol(
                &PathBuf::from(format!("c.{ext}")),
                "ambiguity_demo",
                &[],
                ResolveTarget::Callable,
            );
            assert!(out.is_empty(), "{ext}: ambiguous bare call must not emit");

            let decisions = r.take_decisions().unwrap();
            let last = decisions.last().expect("a decision was recorded");
            assert_eq!(
                last.tier,
                DecisionTier::AmbiguousGlobal,
                "{ext}: Tier-3 with ≥2 kind-filtered candidates must record \
                 AmbiguousGlobal, got {:?} (alt_count={})",
                last.tier,
                last.alt_count
            );
            assert!(
                last.alt_count >= 2,
                "{ext}: alt_count should reflect the candidate set, got {}",
                last.alt_count
            );
            assert!(last.target_id.is_none(), "{ext}: must not pick a target");
        }
    }

    #[test]
    fn tier3_zero_candidates_still_records_unresolved() {
        // Empty symbol table → bare name truly has no matches → keep the
        // `Unresolved` decision (NOT AmbiguousGlobal). Pins that the new
        // variant only fires on the "found, suppressed" path.
        let st = st_with(&[]);
        let mut r = Resolver::new(&st);
        r.enable_dump();
        let out = r.resolve_symbol(
            &PathBuf::from("a.rs"),
            "nonexistent",
            &[],
            ResolveTarget::Callable,
        );
        assert!(out.is_empty());

        let last = r.take_decisions().unwrap().pop().unwrap();
        assert_eq!(last.tier, DecisionTier::Unresolved);
        assert_eq!(last.alt_count, 0);
    }

    #[test]
    fn tier3_unique_kind_filter_recovers_does_not_surface_ambiguity() {
        // One Function + one Variable share the name. Callable target filters
        // out the Variable → uniqueness restored → Global decision wins.
        // Pins that AmbiguousGlobal only fires when the *post-filter* set is
        // ≥2, not when the raw same-name set is ≥2.
        let st = st_with(&[
            ("a.rs", "config", NodeKind::Function),
            ("b.rs", "config", NodeKind::Variable),
        ]);
        let mut r = Resolver::new(&st);
        r.enable_dump();
        let out = r.resolve_symbol(
            &PathBuf::from("c.rs"),
            "config",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(out, vec![(0, ResolutionTier::Global.base_confidence())]);

        let last = r.take_decisions().unwrap().pop().unwrap();
        assert_eq!(last.tier, DecisionTier::Global);
    }

    // ── Test doubles ─────────────────────────────────────────────────────────

    #[test]
    fn tier3_test_double_keeps_production_name_ambiguous_across_14_langs() {
        // Pins the measured decision: a test fake sharing a production
        // method's name keeps every bare call to it AmbiguousGlobal. A
        // production-preferring tie-break wired driver `conn.execute` calls
        // to a project `execute`; receiver typing is the fix for those.
        let langs: &[&str] = &[
            "ts", "js", "py", "java", "kt", "cs", "go", "rs", "php", "rb", "swift", "c", "cpp",
            "dart",
        ];
        for ext in langs {
            let st = st_with(&[
                (
                    Box::leak(format!("src/service.{ext}").into_boxed_str()),
                    "scan_range",
                    NodeKind::Function,
                ),
                (
                    Box::leak(format!("tests/fakes.{ext}").into_boxed_str()),
                    "scan_range",
                    NodeKind::Function,
                ),
            ]);
            let mut r = Resolver::new(&st);
            r.enable_dump();
            let out = r.resolve_symbol(
                &PathBuf::from(format!("src/search.{ext}")),
                "scan_range",
                &[],
                ResolveTarget::Callable,
            );
            assert!(out.is_empty(), "{ext}: got {out:?}");
            let last = r.take_decisions().unwrap().pop().unwrap();
            assert_eq!(last.tier, DecisionTier::AmbiguousGlobal, "{ext}");
        }
    }

    // ── Receiver-typing ladder ──────────────────────────────────────────────

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

    /// Resolve a Callable `callee` from `caller`; return the edges and the
    /// tier of the last recorded decision.
    fn resolve_dumped(
        st: &SymbolTable,
        caller: &str,
        callee: &str,
        imports: &[RawImport],
    ) -> (Vec<(NodeId, f32)>, DecisionTier) {
        let mut r = Resolver::new(st);
        r.enable_dump();
        let out = r.resolve_symbol(
            &PathBuf::from(caller),
            callee,
            imports,
            ResolveTarget::Callable,
        );
        let last = r.take_decisions().unwrap().pop().expect("a decision");
        (out, last.tier)
    }

    fn import(source: &str, name: &str, alias: Option<&str>) -> RawImport {
        RawImport {
            source: source.to_string(),
            imported_name: name.to_string(),
            alias: alias.map(str::to_string),
            binding_kind: None,
        }
    }

    const HERITAGE: f32 = 0.8;

    /// `pkg/base.py: class Base: def greet`, `pkg/derived.py: class Derived`,
    /// and a decoy `greet` on an unrelated type. Ids: Base 0, Base.greet 1,
    /// Derived 2, Decoy 3, Decoy.greet 4.
    fn inherited_greet() -> SymbolTable {
        st_owned(&[
            ("pkg/base.py", "Base", NodeKind::Class, None),
            ("pkg/base.py", "greet", NodeKind::Method, Some("Base")),
            ("pkg/derived.py", "Derived", NodeKind::Class, None),
            ("pkg/decoy.py", "Decoy", NodeKind::Class, None),
            ("pkg/decoy.py", "greet", NodeKind::Method, Some("Decoy")),
        ])
    }

    #[test]
    fn test_resolve_member_via_type_member_in_sibling_impl_file_returns_type_owned() {
        let st = st_owned(&[
            ("src/model.rs", "Repo", NodeKind::Struct, None),
            ("src/repo_impl.rs", "save", NodeKind::Method, Some("Repo")),
            ("src/decoy.rs", "Decoy", NodeKind::Struct, None),
            ("src/decoy.rs", "save", NodeKind::Method, Some("Decoy")),
        ]);
        let (out, tier) = resolve_dumped(&st, "src/app.rs", "Repo.save", &[]);
        assert_eq!(
            out,
            vec![(1, ResolutionTier::QualifierScoped.base_confidence())]
        );
        assert_eq!(tier, DecisionTier::TypeOwned);
    }

    #[test]
    fn test_resolve_member_via_type_member_on_resolved_base_returns_type_heritage() {
        let mut st = inherited_greet();
        st.build_supertypes([(2, Some(0))]);
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "Derived.greet", &[]);
        assert_eq!(out, vec![(1, HERITAGE)]);
        assert_eq!(tier, DecisionTier::TypeHeritage);
    }

    #[test]
    fn test_resolve_member_via_type_no_supertypes_returns_unresolved() {
        let st = inherited_greet();
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "Derived.greet", &[]);
        assert!(out.is_empty(), "closed heritage, no owner: got {out:?}");
        assert_eq!(tier, DecisionTier::Unresolved);
    }

    #[test]
    fn test_resolve_member_via_type_external_base_before_owner_returns_no_edge() {
        // `class Derived(ExternalMixin, Base)`: the mixin comes first in the
        // MRO and may define `greet`.
        let mut st = inherited_greet();
        st.build_supertypes([(2, None), (2, Some(0))]);
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "Derived.greet", &[]);
        assert!(out.is_empty(), "got {out:?}");
        assert_eq!(tier, DecisionTier::Unresolved);
    }

    #[test]
    fn test_resolve_member_via_type_external_base_after_owner_returns_type_heritage() {
        let mut st = inherited_greet();
        st.build_supertypes([(2, Some(0)), (2, None)]);
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "Derived.greet", &[]);
        assert_eq!(out, vec![(1, HERITAGE)]);
        assert_eq!(tier, DecisionTier::TypeHeritage);
    }

    #[test]
    fn test_resolve_member_via_type_owners_on_two_branches_returns_no_edge() {
        // `class T(A, B)` with `greet` on both A and B: the winner depends on
        // the language's method resolution order.
        let mut st = st_owned(&[
            ("pkg/a.py", "A", NodeKind::Class, None),
            ("pkg/a.py", "greet", NodeKind::Method, Some("A")),
            ("pkg/b.py", "B", NodeKind::Class, None),
            ("pkg/b.py", "greet", NodeKind::Method, Some("B")),
            ("pkg/t.py", "T", NodeKind::Class, None),
        ]);
        st.build_supertypes([(4, Some(0)), (4, Some(2))]);
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "T.greet", &[]);
        assert!(out.is_empty(), "got {out:?}");
        assert_eq!(tier, DecisionTier::Unresolved);
    }

    #[test]
    fn test_resolve_member_via_type_override_chain_returns_most_derived_owner() {
        // T -> A -> B, both A and B define `greet`: A overrides B.
        let mut st = st_owned(&[
            ("pkg/a.py", "A", NodeKind::Class, None),
            ("pkg/a.py", "greet", NodeKind::Method, Some("A")),
            ("pkg/b.py", "B", NodeKind::Class, None),
            ("pkg/b.py", "greet", NodeKind::Method, Some("B")),
            ("pkg/t.py", "T", NodeKind::Class, None),
        ]);
        st.build_supertypes([(4, Some(0)), (0, Some(2))]);
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "T.greet", &[]);
        assert_eq!(out, vec![(1, HERITAGE)]);
        assert_eq!(tier, DecisionTier::TypeHeritage);
    }

    /// Two `Repo` types in sibling packages. Ids: a.Repo 0, a.Repo.save 1,
    /// b.Repo 2, b.Repo.load 3.
    fn two_repos() -> SymbolTable {
        st_owned(&[
            ("app/a/models.py", "Repo", NodeKind::Class, None),
            ("app/a/models.py", "save", NodeKind::Method, Some("Repo")),
            ("app/b/models.py", "Repo", NodeKind::Class, None),
            ("app/b/models.py", "load", NodeKind::Method, Some("Repo")),
        ])
    }

    #[test]
    fn test_resolve_member_via_type_ambiguous_qualifier_one_owner_returns_type_candidates() {
        let st = two_repos();
        let (out, tier) = resolve_dumped(&st, "app/c.py", "Repo.save", &[]);
        assert_eq!(out, vec![(1, ResolutionTier::Global.base_confidence())]);
        assert_eq!(tier, DecisionTier::TypeCandidates);
    }

    #[test]
    fn test_resolve_member_via_type_ambiguous_qualifier_one_unknown_returns_ambiguous_global() {
        // b.Repo has an external base that may define `save`.
        let mut st = two_repos();
        st.build_supertypes([(2, None)]);
        let (out, tier) = resolve_dumped(&st, "app/c.py", "Repo.save", &[]);
        assert!(out.is_empty(), "got {out:?}");
        assert_eq!(tier, DecisionTier::AmbiguousGlobal);
    }

    #[test]
    fn test_resolve_member_via_type_module_qualifier_returns_unresolved() {
        // `cfg` is a module, not a type; a member owned under that name in
        // another file must not resolve through the ladder.
        let st = st_owned(&[
            ("src/lib.rs", "cfg", NodeKind::Module, None),
            ("src/loader.rs", "load", NodeKind::Function, Some("cfg")),
        ]);
        let (out, tier) = resolve_dumped(&st, "src/app.rs", "cfg::load", &[]);
        assert!(out.is_empty(), "got {out:?}");
        assert_eq!(tier, DecisionTier::Unresolved);
    }

    #[test]
    fn test_resolve_member_via_type_library_import_returns_no_edge() {
        // `from psqlpy import Connection`: the project's own `Connection`
        // (whose base defines `execute`) is not this receiver's type.
        let mut st = st_owned(&[
            ("pkg/base.py", "BaseConn", NodeKind::Class, None),
            ("pkg/base.py", "execute", NodeKind::Method, Some("BaseConn")),
            ("pkg/models.py", "Connection", NodeKind::Class, None),
        ]);
        st.build_supertypes([(2, Some(0))]);
        let lib = [import("psqlpy", "Connection", None)];
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "Connection.execute", &lib);
        assert!(out.is_empty(), "got {out:?}");
        assert_eq!(tier, DecisionTier::Unresolved);

        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "Connection.execute", &[]);
        assert_eq!(
            out,
            vec![(1, HERITAGE)],
            "without the library import the project type resolves"
        );
        assert_eq!(tier, DecisionTier::TypeHeritage);
    }

    #[test]
    fn test_resolve_member_via_type_aliased_project_import_uses_declared_name() {
        // `from .models import Repo as R` then `R.save()`.
        let mut st = st_owned(&[
            ("pkg/models.py", "Repo", NodeKind::Class, None),
            ("pkg/base.py", "BaseRepo", NodeKind::Class, None),
            ("pkg/base.py", "save", NodeKind::Method, Some("BaseRepo")),
        ]);
        st.build_supertypes([(0, Some(1))]);
        let aliased = [import(".models", "Repo", Some("R"))];
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "R.save", &aliased);
        assert_eq!(out, vec![(2, HERITAGE)]);
        assert_eq!(tier, DecisionTier::TypeHeritage);
    }

    #[test]
    fn test_resolve_member_via_type_external_path_prefix_returns_unresolved() {
        // `std::sync::Arc::new` names std's Arc, not a project `Arc`.
        let st = st_owned(&[
            ("src/arc.rs", "Arc", NodeKind::Struct, None),
            ("src/arc_impl.rs", "new", NodeKind::Function, Some("Arc")),
        ]);
        let (out, tier) = resolve_dumped(&st, "src/app.rs", "std::sync::Arc::new", &[]);
        assert!(out.is_empty(), "got {out:?}");
        assert_eq!(tier, DecisionTier::Unresolved);
    }
}
