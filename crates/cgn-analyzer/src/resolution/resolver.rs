use cgn_core::analyzer::types::RawImport;
use serde::Serialize;
use std::path::Path;
use std::sync::Mutex;

use crate::resolution::heuristics::ResolutionTier;
use crate::resolution::index::{FileMeta, ResolveTarget, SymbolTable};
use crate::resolution::path_aliases::PathAliases;
use crate::rust::module_tree::RustWorkspaceModTree;

pub type NodeId = u32;

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
        let source_file_str = source_file.to_string_lossy();

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
        // workspace crate index can distinguish `cgn_core::...` (internal,
        // safe to fall back) from `std::...` (external, refuse).
        if let Some((qualifier, member)) = split_qualifier(symbol_name) {
            let hit = self
                .resolve_qualifier_file(source_file, qualifier, raw_imports, Some(symbol_name))
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

            self.record(
                &source_file_str,
                symbol_name,
                None,
                DecisionTier::Unresolved,
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
                if let Some(qf) = self.resolve_qualifier_file(source_file, base, raw_imports, None)
                {
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

        if let Some(node_id) =
            self.symbol_table
                .lookup_unique_global(symbol_name, target, caller_meta)
        {
            results.push((node_id, ResolutionTier::Global.base_confidence()));
            self.record(
                &source_file_str,
                symbol_name,
                specifier,
                DecisionTier::Global,
                Some(node_id),
                raw_count.saturating_sub(1),
                Some(ResolutionTier::Global.base_confidence()),
            );
        } else {
            // Distinguish "no candidates" from "≥2 candidates, suppressed
            // by the unique-only cap". The post-filter count walks the
            // same predicates as `lookup_unique_global` so the two views
            // can't drift. Cost paid only on miss (Tier 3 hits skip this).
            let filtered =
                self.symbol_table
                    .count_global_kind_filtered(symbol_name, target, caller_meta);
            let tier = if filtered >= 2 {
                DecisionTier::AmbiguousGlobal
            } else {
                DecisionTier::Unresolved
            };
            self.record(
                &source_file_str,
                symbol_name,
                specifier,
                tier,
                None,
                raw_count,
                None,
            );
        }

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
fn crate_root_prefix(path: &str) -> &str {
    path.rsplit_once("/src/")
        .or_else(|| path.rsplit_once("/tests/"))
        .map(|(root, _)| root)
        .unwrap_or("")
}

#[cfg(windows)]
fn crate_root_prefix(path: &str) -> &str {
    // Windows paths use backslashes natively.
    path.rsplit_once("\\src\\")
        .or_else(|| path.rsplit_once("\\tests\\"))
        .or_else(|| path.rsplit_once("/src/")) // Fallback for mixed/normalized paths
        .or_else(|| path.rsplit_once("/tests/"))
        .map(|(root, _)| root)
        .unwrap_or("")
}

fn split_qualifier(name: &str) -> Option<(&str, &str)> {
    let colon_idx = name.rfind("::");
    let dot_idx = name.rfind('.');
    let (sep_len, split_idx) = match (colon_idx, dot_idx) {
        (Some(c), Some(d)) if c >= d => (2usize, c),
        (Some(_), Some(d)) => (1, d),
        (Some(c), None) => (2, c),
        (None, Some(d)) => (1, d),
        (None, None) => return None,
    };
    let (before, after) = name.split_at(split_idx);
    let member = &after[sep_len..];
    if before.is_empty() || member.is_empty() {
        return None;
    }
    let qualifier = before
        .rsplit_once("::")
        .or_else(|| before.rsplit_once('.'))
        .map(|(_, q)| q)
        .unwrap_or(before);
    if qualifier.is_empty() {
        return None;
    }
    Some((qualifier, member))
}

/// Extensions probed during L0 candidate enumeration (covers every
/// language whose parser is wired into cgn-analyzer).
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

    probe_with_suffixes(&base, &mut visit);
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
        raw_imports: &[RawImport],
        full_callee: Option<&str>,
    ) -> Option<String> {
        let source_file_str = source_file.to_string_lossy();

        // Tier 1: same-file qualifier definition. Qualifiers are class /
        // interface names, so filter to Type here — avoids a property
        // named `Logger` winning over `class Logger` in the same file.
        if let Some(id) = self.symbol_table.lookup_in_file_with_kind(
            &source_file_str,
            qualifier,
            ResolveTarget::Type,
        ) {
            return self.symbol_table.file_of(id).map(str::to_string);
        }

        // Tier 2: imported qualifier (matches alias or imported_name; expands
        // specifier via the same L0 candidate enumeration used by the bare-
        // name resolver).
        for import in raw_imports {
            let matches_qualifier = match &import.alias {
                Some(alias) => alias == qualifier,
                None => import.imported_name == qualifier,
            };
            if !matches_qualifier {
                continue;
            }
            let exported = &import.imported_name;
            let mut hit: Option<String> = None;
            for_each_specifier_candidate(
                source_file,
                &import.source,
                &self.path_aliases,
                |candidate| {
                    if self
                        .symbol_table
                        .lookup_in_file_with_kind(candidate, exported, ResolveTarget::Type)
                        .is_some()
                    {
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
        let caller_meta = FileMeta::from_path(&source_file_str);
        if let Some(id) =
            self.symbol_table
                .lookup_unique_global(qualifier, ResolveTarget::Type, caller_meta)
        {
            return self.symbol_table.file_of(id).map(str::to_string);
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

    use cgn_core::graph::NodeKind;

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
            &PathBuf::from("crates/cgn-cli/src/main.rs"),
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
        use cgn_core::analyzer::types::RawImport;
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
}
