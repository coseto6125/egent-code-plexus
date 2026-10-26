//! Rust workspace module-tree builder and FQN resolver.
//!
//! Resolves `crate::a::b::fn_name` (and `<crate_name>::a::b::fn_name`) to
//! the canonical file path that declares `fn_name`, by walking the module
//! tree from each crate root.
//!
//! # Resolution model (rs_oracle.py spec, plus `[lib]` overrides)
//!
//! * `crate::a::b::Foo` (in crate X) → walk X's mod tree starting from the
//!   crate entry (`[lib] path`, else `src/lib.rs` / `src/main.rs`), following
//!   `mod foo;` decls.
//! * `<crate_name>::a::b::Foo` → if `crate_name` is a workspace member,
//!   resolve through its mod tree. The name is the `[lib] name` when the
//!   manifest sets one; rs_oracle.py knows only the package name.
//! * `super::Foo` / `self::Foo` → resolved against caller's module path.
//! * `std::*` / `core::*` / `alloc::*` → external, no file.
//! * `pub use inner::Foo` re-export chains are walked transitively (max 16
//!   hops, cycle-safe). The walk and the chain are the rule the overlay
//!   shares ([`ecp_core::analyzer::ladder::module_tree_item`]); this module
//!   answers its questions ([`IndexModuleTree`]).
//!
//! # Out of scope (documented)
//! * `cfg`-gated `mod` decls (first filesystem hit wins).
//! * `#[path = "..."]` overrides.
//! * Macro-expanded `use`s.
//! * `extern crate` re-exports (legacy syntax).
//! * Proc-macro-generated re-exports (`paste!`, `derive_more`, etc.) — logged
//!   as BlindSpot if encountered.

use ecp_core::analyzer::ladder::{self, Published, ReExport, TreeItem};
use ecp_core::analyzer::rust_paths::rust_module_dir;
use ecp_core::contracts::workspace::{
    ConfigPresence, ConfigRead, Ecosystem, Membership, NameRank, Scope, ScopeId, ScopeName,
    WorkspaceFacts,
};
use ecp_core::registry::uid_path;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Regex helpers (compile-once per call via OnceLock)
// ---------------------------------------------------------------------------

use std::sync::OnceLock;

fn mod_decl_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?m)^\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;")
            .expect("mod_decl_re")
    })
}

fn path_attr_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r#"#\[path\s*=\s*"([^"]+)"\]"#).expect("path_attr_re"))
}

fn line_comment_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"//[^\n]*").expect("line_comment_re"))
}

fn block_comment_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?s)/\*.*?\*/").expect("block_comment_re"))
}

/// Matches `pub use <path>::<Name>` and `pub use <path>::<Name> as <Alias>`.
/// Also matches `pub use <path>::*` for glob re-exports.
///
/// Capture groups:
///   1: module path prefix (everything before the last `::`)
///   2: exported item name (last segment, or `*` for glob)
///   3: optional alias after `as`
///
/// Visibility variants (`pub(crate)`, `pub(super)`, etc.) are all accepted
/// because all make the item reachable from *some* external scope.
fn pub_use_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?m)^\s*pub(?:\s*\([^)]*\))?\s+use\s+((?:[A-Za-z_][A-Za-z0-9_]*::)+)(\*|[A-Za-z_][A-Za-z0-9_]*)(?:\s+as\s+([A-Za-z_][A-Za-z0-9_]*))?\s*;"
        )
        .expect("pub_use_re")
    })
}

/// Matches the one-level group form `pub use <path>::{A, b::C, D as E};`.
/// Capture 1 is the shared prefix (with its trailing `::`), capture 2 the
/// brace body. A nested group inside the braces does not match, so that
/// statement keeps its pre-group behaviour (not collected).
fn pub_use_group_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?m)^\s*pub(?:\s*\([^)]*\))?\s+use\s+((?:[A-Za-z_][A-Za-z0-9_]*::)+)\{([^{};]*)\}\s*;"
        )
        .expect("pub_use_group_re")
    })
}

/// Strip `//` and `/* */` comments from source, preserving newlines so
/// MULTILINE regex anchors keep working.
fn strip_comments(src: &str) -> String {
    let s = block_comment_re().replace_all(src, |caps: &regex::Captures| {
        caps[0]
            .chars()
            .map(|c| if c == '\n' { '\n' } else { ' ' })
            .collect::<String>()
    });
    line_comment_re()
        .replace_all(&s, |caps: &regex::Captures| {
            caps[0]
                .chars()
                .map(|c| if c == '\n' { '\n' } else { ' ' })
                .collect::<String>()
        })
        .into_owned()
}

// ---------------------------------------------------------------------------
// Module tree
// ---------------------------------------------------------------------------

/// Maps `mod_path` (as a `Vec<String>`) to the canonical absolute file path.
/// The crate root itself maps to an empty vec `[]`.
type ModTree = FxHashMap<Vec<String>, PathBuf>;

/// A single `pub use` re-export entry collected from a source file.
#[derive(Debug, Clone)]
struct ReExportEntry {
    /// The path prefix segments before the last `::` (e.g. `["inner"]` for
    /// `pub use inner::Widget`). May start with `crate`, `super`, `self`, or
    /// a crate name.
    path_prefix: Vec<String>,
    /// The original item name at the source (last segment before `as`),
    /// `*` for a glob re-export (`pub use path::*`).
    original_name: String,
    /// The module path of the file that contains this `pub use` statement.
    /// Used to resolve relative prefix segments (e.g. `nested` in
    /// `src/deep/mod.rs` means `deep::nested`, not root-level `nested`).
    containing_mod_path: Vec<String>,
}

/// Per-crate map from `(canonical_abs_file, exported_symbol_name)` to the
/// re-export entry that publishes it. `exported_symbol_name` is the name
/// visible at the re-export site (i.e. after `as Alias` renaming, or the
/// original name when no alias is given, or `"*"` for glob entries).
type ReExportMap = FxHashMap<(PathBuf, String), ReExportEntry>;

/// Workspace-level Rust module tree: one `ModTree` per crate.
pub struct RustWorkspaceModTree {
    /// Canonicalized absolute workspace root, computed once at build to
    /// keep per-call resolution off the `canonicalize` syscall path.
    workspace_canon: PathBuf,
    /// `crate_name → (crate_dir, canonical_crate_dir_string, ModTree, ReExportMap)`.
    /// `canonical_crate_dir_string` is the forward-slash-normalised
    /// canonical path of the crate directory — cached so `crate_for_file`
    /// doesn't re-canonicalize N crates on every resolution.
    /// Keyed by the name code writes (the `[lib] name`, else the package name
    /// with `-` → `_`), plus the package-name spellings where no other
    /// crate's code name takes them.
    crates: FxHashMap<String, (PathBuf, String, ModTree, ReExportMap)>,
    /// Maps an absolute canonical file path back to `(crate_name, mod_path)`,
    /// `crate_name` being the code-name key of `crates`.
    file_to_crate: FxHashMap<PathBuf, (String, Vec<String>)>,
    /// One module tree per Cargo target: each crate's primary tree (the one
    /// in `crates`), then its bin roots.
    targets: Vec<TargetTree>,
    /// Repo-relative file → each `(index into targets, mod_path)` that
    /// declares it. A module both the lib and a bin declare has two entries.
    file_targets: FxHashMap<String, Vec<(usize, Vec<String>)>>,
    /// Contract C2's Cargo rows, from the manifest parse above: one scope
    /// per workspace member, and the names that reach it.
    workspace: WorkspaceFacts,
}

/// One Cargo target's modules, `mod_path → repo-relative file`.
struct TargetTree {
    /// Set for a bin entry beside the crate's primary entry (`src/main.rs`
    /// next to `src/lib.rs`, `src/bin/*.rs`), whose modules `file_to_crate`
    /// does not cover: the bin's own module tree and `pub use` map, keyed
    /// like a crate's, so its re-export chains can be followed.
    bin_scope: Option<(ModTree, ReExportMap)>,
    modules: FxHashMap<Vec<String>, String>,
}

impl RustWorkspaceModTree {
    /// Build the workspace module tree rooted at `workspace_root`.
    /// Silently skips missing / unreadable files.
    pub fn build(workspace_root: &Path) -> Self {
        let workspace_canon = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        let mut out = Self {
            workspace_canon,
            crates: FxHashMap::default(),
            file_to_crate: FxHashMap::default(),
            targets: Vec::new(),
            file_targets: FxHashMap::default(),
            workspace: WorkspaceFacts::default(),
        };
        let root_toml = workspace_root.join("Cargo.toml");
        let Some(raw) = read_file(&root_toml) else {
            return out;
        };

        let members = parse_workspace_members(&raw, workspace_root);
        let members = if members.is_empty() {
            vec![workspace_root.to_path_buf()]
        } else {
            members
        };
        let mut fallbacks = Vec::new();
        let mut rows = CargoRows::default();
        for crate_dir in members {
            let manifest = if crate_dir == workspace_root {
                Some(raw.clone())
            } else {
                read_file(&crate_dir.join("Cargo.toml"))
            };
            let Some(manifest) = manifest.as_deref().and_then(read_crate_manifest) else {
                continue;
            };
            let Some(entry_path) = find_crate_entry(&crate_dir, manifest.lib_path.as_deref())
            else {
                continue;
            };
            let (tree, reexports) = build_mod_tree_and_reexports(&entry_path);
            out.add_target(&tree);
            let bins = bin_roots(&crate_dir, &entry_path);
            let crate_canon = crate_dir.canonicalize().ok();
            rows.add_member(
                &out.workspace_canon,
                &crate_dir,
                crate_canon.as_deref(),
                crate_dir != workspace_root,
                std::iter::once(&entry_path).chain(&bins),
                &manifest,
            );
            for bin_root in bins {
                let bin_scope = build_mod_tree_and_reexports(&bin_root);
                let index = out.add_target(&bin_scope.0);
                out.targets[index].bin_scope = Some(bin_scope);
            }
            // Code names a crate by its lib target: the `[lib] name`, else the
            // package name with `-` → `_`. That key wins every collision; the
            // package-name spellings only fill keys nothing else claims.
            let package_norm = manifest.name.replace('-', "_");
            let code_name = manifest.lib_name.unwrap_or_else(|| package_norm.clone());
            for (mod_path, file) in &tree {
                out.file_to_crate
                    .insert(file.clone(), (code_name.clone(), mod_path.clone()));
            }
            let canon_str = crate_canon
                .as_deref()
                .unwrap_or(&crate_dir)
                .to_string_lossy()
                .replace('\\', "/");
            let entry = (crate_dir, canon_str, tree, reexports);
            // Aliases need their own tree entry because lookups split by
            // crate name first. One clone per alias is unavoidable; tree size
            // is bounded by mod-tree depth × crate count.
            for alias in [manifest.name, package_norm] {
                if alias != code_name && !fallbacks.iter().any(|(a, _)| *a == alias) {
                    fallbacks.push((alias, entry.clone()));
                }
            }
            out.crates.insert(code_name, entry);
        }
        for (alias, entry) in fallbacks {
            out.crates.entry(alias).or_insert(entry);
        }
        out.workspace = rows.finish();

        out
    }

    /// Contract C2's Cargo rows of this workspace, for `graph.bin`. Empty
    /// without a root `Cargo.toml`.
    pub fn workspace_facts(&self) -> &WorkspaceFacts {
        &self.workspace
    }

    /// Resolve a Rust FQN call to the file path (repo-relative) of the
    /// module that declares the terminal item, by the module-tree rule the
    /// overlay shares ([`ladder::module_tree_item`]).
    ///
    /// `full_callee` is the raw call string as captured by the parser, e.g.
    /// `"crate::build::orchestrator::build_l2"`.
    /// `caller_file` is the repo-relative path of the file containing the call.
    /// `workspace_root` is used to make absolute paths relative again.
    ///
    /// Returns `None` if the path cannot be resolved (external crate, unknown
    /// module, or the target file cannot be found on disk).
    pub fn resolve_fqn(
        &self,
        full_callee: &str,
        caller_file: &str,
        workspace_root: &Path,
    ) -> Option<ResolvedFqn> {
        match ladder::module_tree_item(&self.facts(workspace_root), caller_file, full_callee)? {
            TreeItem::Found { file, item } => Some(ResolvedFqn {
                file,
                item_name: item,
            }),
            TreeItem::Unknown => None,
        }
    }

    /// These trees as [`ladder::ModuleTreeFacts`], for callers named
    /// relative to `workspace_root`.
    pub fn facts<'t>(&'t self, workspace_root: &'t Path) -> IndexModuleTree<'t> {
        IndexModuleTree {
            tree: self,
            workspace_root,
        }
    }

    /// The repo-relative file of the module that `module_path` (`crate` /
    /// `self` / `super`-anchored, e.g. `super::util`) names from
    /// `caller_file`, read from the tree of each target that declares the
    /// caller: `crate` in a module of `src/main.rs` is `main.rs`, and
    /// `super` in a `#[path]` file is the module that declares it. `None`
    /// when no target declares the caller, or one of them does not place
    /// the module, or they place it at different files (a module both the
    /// lib and a bin declare).
    pub fn anchored_module_file(&self, caller_file: &str, module_path: &str) -> Option<&str> {
        let segs: Vec<&str> = module_path.split("::").collect();
        let mut found: Option<&str> = None;
        for (target, mod_path) in self.file_targets.get(caller_file)? {
            let key = anchored_mod_path(mod_path, &segs)?;
            let file = self.targets[*target].modules.get(&key)?;
            if found.is_some_and(|f| f != file.as_str()) {
                return None;
            }
            found = Some(file.as_str());
        }
        found
    }

    /// The tree and module path of `caller_file` when exactly one target
    /// declares it and that target is a bin beside the crate's primary
    /// entry: `crate::` there is the bin's root, which `file_to_crate`,
    /// built from the primary trees alone, does not hold.
    fn bin_target(&self, caller_file: &str) -> Option<(TreeScope<'_>, &[String])> {
        let [(target, mod_path)] = self.file_targets.get(caller_file)?.as_slice() else {
            return None;
        };
        let (tree, reexports) = self.targets[*target].bin_scope.as_ref()?;
        Some((TreeScope { tree, reexports }, mod_path))
    }

    /// Register `tree`'s modules as one Cargo target; returns its index in
    /// `targets`.
    fn add_target(&mut self, tree: &ModTree) -> usize {
        let index = self.targets.len();
        let mut modules = FxHashMap::default();
        for (mod_path, file) in tree {
            let Ok(rel) = uid_path(file, &self.workspace_canon) else {
                continue;
            };
            self.file_targets
                .entry(rel.clone())
                .or_default()
                .push((index, mod_path.clone()));
            modules.insert(mod_path.clone(), rel);
        }
        self.targets.push(TargetTree {
            bin_scope: None,
            modules,
        });
        index
    }

    /// Returns the canonical crate name for a caller file path.
    ///
    /// Strategy: for each registered crate, check whether the caller's
    /// absolute path is under the crate's root directory (longest-prefix
    /// wins). Uses pre-canonicalized crate strings cached at build time
    /// so this fires zero filesystem syscalls per resolution.
    fn crate_for_file(&self, caller_file: &str, workspace_root: &Path) -> Option<String> {
        let caller_abs = if Path::new(caller_file).is_absolute() {
            PathBuf::from(caller_file)
        } else {
            workspace_root.join(caller_file)
        };
        let caller_canon = caller_abs.canonicalize().unwrap_or(caller_abs);
        let caller_str = caller_canon.to_string_lossy().replace('\\', "/");

        let mut best: Option<(usize, String)> = None;
        for (name, (_crate_dir, cdir_str, _tree, _reexports)) in &self.crates {
            let is_match = caller_str == *cdir_str
                || caller_str.as_bytes().get(cdir_str.len()).copied() == Some(b'/')
                    && caller_str.starts_with(cdir_str);
            if is_match {
                let len = cdir_str.len();
                if best.as_ref().map(|(l, _)| *l < len).unwrap_or(true) {
                    best = Some((len, name.clone()));
                }
            }
        }
        best.map(|(_, name)| name)
    }

    /// The primary tree of the crate keyed `name` in `crates`.
    fn crate_scope(&self, name: &str) -> Option<TreeScope<'_>> {
        let (_, _, tree, reexports) = self.crates.get(name)?;
        Some(TreeScope { tree, reexports })
    }

    /// Whether any crates were indexed.
    pub fn is_empty(&self) -> bool {
        self.crates.is_empty()
    }
}

/// One Cargo target's module tree and `pub use` map: a crate's primary
/// tree, or a bin's.
#[derive(Clone, Copy)]
pub struct TreeScope<'t> {
    tree: &'t ModTree,
    reexports: &'t ReExportMap,
}

/// The index's module trees as [`ladder::ModuleTreeFacts`]: a module is the
/// canonical file its tree holds, a caller path is relative to
/// `workspace_root`.
pub struct IndexModuleTree<'t> {
    tree: &'t RustWorkspaceModTree,
    workspace_root: &'t Path,
}

impl<'t> ladder::ModuleTreeFacts for IndexModuleTree<'t> {
    type Root = TreeScope<'t>;
    type Module = &'t Path;
    type File = String;

    fn crate_root(&self, name: &str) -> Option<TreeScope<'t>> {
        self.tree.crate_scope(name)
    }

    /// A bin-only caller's bin, else the crate whose directory holds it.
    fn caller_crate(&self, caller: &str) -> Option<TreeScope<'t>> {
        if let Some((scope, _)) = self.tree.bin_target(caller) {
            return Some(scope);
        }
        let name = self.tree.crate_for_file(caller, self.workspace_root)?;
        self.tree.crate_scope(&name)
    }

    /// A bin-only caller's bin, else the crate whose directory holds it with
    /// the module path a primary tree gives the caller.
    fn caller_module(&self, caller: &str) -> Option<(TreeScope<'t>, Vec<String>)> {
        if let Some((scope, mod_path)) = self.tree.bin_target(caller) {
            return Some((scope, mod_path.to_vec()));
        }
        let name = self.tree.crate_for_file(caller, self.workspace_root)?;
        let caller_abs = if Path::new(caller).is_absolute() {
            PathBuf::from(caller)
        } else {
            self.workspace_root.join(caller)
        };
        let caller_canon = caller_abs.canonicalize().ok()?;
        let (_, mod_path) = self.tree.file_to_crate.get(&caller_canon)?;
        Some((self.tree.crate_scope(&name)?, mod_path.clone()))
    }

    fn root_module(&self, root: TreeScope<'t>) -> Option<&'t Path> {
        root.tree.get::<[String]>(&[]).map(PathBuf::as_path)
    }

    fn submodule(
        &self,
        root: TreeScope<'t>,
        _parent: &'t Path,
        path: &[String],
    ) -> Option<&'t Path> {
        root.tree.get(path).map(PathBuf::as_path)
    }

    /// The `pub use` naming `name`, else a glob `pub use` of the module.
    fn published(&self, root: TreeScope<'t>, module: &'t Path, name: &str) -> Published<'_> {
        let entry = |name: &str| {
            root.reexports
                .get(&(module.to_path_buf(), name.to_string()))
        };
        match entry(name).or_else(|| entry("*")) {
            Some(entry) => Published::ReExport(ReExport {
                prefix: &entry.path_prefix,
                original: &entry.original_name,
                containing: &entry.containing_mod_path,
            }),
            None => Published::Own,
        }
    }

    fn module_file(&self, module: &'t Path) -> Option<String> {
        uid_path(module, &self.tree.workspace_canon).ok()
    }
}

/// The module path that the `crate` / `self` / `super`-anchored `segs` name
/// from the module at `mod_path`; `None` for another head, or a `super`
/// chain that climbs above the crate root.
fn anchored_mod_path(mod_path: &[String], segs: &[&str]) -> Option<Vec<String>> {
    let (base, rest) = match *segs.first()? {
        "crate" => (&mod_path[..0], &segs[1..]),
        "self" => (mod_path, &segs[1..]),
        "super" => {
            let up = segs.iter().take_while(|&&s| s == "super").count();
            (&mod_path[..mod_path.len().checked_sub(up)?], &segs[up..])
        }
        _ => return None,
    };
    Some(
        base.iter()
            .cloned()
            .chain(rest.iter().map(|s| s.to_string()))
            .collect(),
    )
}

/// Cargo's auto-discovered bin roots, `src/main.rs`, `src/bin/*.rs` and
/// `src/bin/*/main.rs`, other than the crate's primary `entry`.
fn bin_roots(crate_dir: &Path, entry: &Path) -> Vec<PathBuf> {
    let src = crate_dir.join("src");
    let mut roots = vec![src.join("main.rs")];
    if let Ok(dir) = std::fs::read_dir(src.join("bin")) {
        let mut bins: Vec<PathBuf> = dir
            .flatten()
            .map(|e| {
                let p = e.path();
                if p.is_dir() {
                    p.join("main.rs")
                } else {
                    p
                }
            })
            .collect();
        bins.sort();
        roots.extend(bins);
    }
    roots.retain(|p| {
        p.as_path() != entry && p.extension().is_some_and(|e| e == "rs") && p.is_file()
    });
    roots
}

/// Result of a successful FQN resolution.
#[derive(Debug, Clone)]
pub struct ResolvedFqn {
    /// Repo-relative file path (forward slashes).
    pub file: String,
    /// The terminal item name (last FQN segment).
    pub item_name: String,
}

// ---------------------------------------------------------------------------
// Mod-tree BFS
// ---------------------------------------------------------------------------

/// Walk from `entry` and build a `(ModTree, ReExportMap)` for the crate.
fn build_mod_tree_and_reexports(entry: &Path) -> (ModTree, ReExportMap) {
    let mut tree: ModTree = FxHashMap::default();
    let mut reexports: ReExportMap = FxHashMap::default();

    let abs_entry = match entry.canonicalize() {
        Ok(p) => p,
        Err(_) => entry.to_path_buf(),
    };
    tree.insert(vec![], abs_entry.clone());

    let mut stack: Vec<(Vec<String>, PathBuf)> = vec![(vec![], abs_entry.clone())];
    let mut visited: std::collections::HashSet<PathBuf> =
        std::collections::HashSet::from([abs_entry]);

    while let Some((mod_path, file)) = stack.pop() {
        let Some(src) = read_file(&file) else {
            continue;
        };
        let clean = strip_comments(&src);

        // Collect `#[path = "..."]` attributes so we can honour them.
        // Simple heuristic: scan for `#[path = "..."]` on the line immediately
        // before a `mod NAME;`. We pass them as a map: mod_name → rel_path.
        let path_attrs = collect_path_attrs(&clean);

        for cap in mod_decl_re().captures_iter(&clean) {
            let name = cap[1].to_string();
            let child_file = if let Some(override_rel) = path_attrs.get(&name) {
                file.parent()
                    .map(|d| d.join(override_rel))
                    .unwrap_or_else(|| PathBuf::from(override_rel))
            } else {
                // The entry is the crate root whatever its file name
                // (`[lib] path = "src/core.rs"`), so its modules sit beside it.
                let found = if mod_path.is_empty() {
                    file.parent().and_then(|dir| module_file_in(dir, &name))
                } else {
                    file_for_mod(&file, &name)
                };
                match found {
                    Some(p) => p,
                    None => continue,
                }
            };
            let child_canon = match child_file.canonicalize() {
                Ok(p) => p,
                Err(_) => {
                    if child_file.exists() {
                        child_file.clone()
                    } else {
                        continue;
                    }
                }
            };
            if !visited.insert(child_canon.clone()) {
                continue;
            }
            let mut child_path = mod_path.clone();
            child_path.push(name);
            tree.insert(child_path.clone(), child_canon.clone());
            stack.push((child_path, child_canon));
        }

        // Collect `pub use` re-exports from this file.
        collect_pub_use_entries(&clean, &file, &mod_path, &mut reexports);
    }
    (tree, reexports)
}

/// Scan `clean` (comments already stripped) for `pub use` statements and
/// record each one in `reexports`, keyed by `(file, exported_name)`.
///
/// For `pub use path::Name as Alias` the key uses `Alias` (the name visible
/// to callers); `original_name` stores `Name` (the source symbol to resolve).
/// For `pub use path::*` the key uses `"*"` as a wildcard sentinel.
///
/// `containing_mod_path` is the module path of `file` within the crate tree
/// (e.g. `["deep"]` for `src/deep/mod.rs`). It is stored in the entry so
/// that relative path prefixes (like `nested` in `pub use nested::W`) can be
/// correctly resolved as absolute crate-rooted paths during chain walking.
fn collect_pub_use_entries(
    clean: &str,
    file: &Path,
    containing_mod_path: &[String],
    reexports: &mut ReExportMap,
) {
    for cap in pub_use_re().captures_iter(clean) {
        // cap[1]: prefix like "inner::" or "crate::deep::" (trailing "::")
        // cap[2]: item name or "*"; cap[3]: optional alias
        insert_pub_use(
            cap[1].split("::"),
            &cap[2],
            cap.get(3).map(|m| m.as_str()),
            file,
            containing_mod_path,
            reexports,
        );
    }
    for cap in pub_use_group_re().captures_iter(clean) {
        for member in cap[2].split(',').map(str::trim).filter(|m| !m.is_empty()) {
            let (path, alias) = match member.split_once(" as ") {
                Some((path, alias)) => (path.trim(), Some(alias.trim())),
                None => (member, None),
            };
            let (sub, item) = match path.rsplit_once("::") {
                Some((sub, item)) => (Some(sub), item),
                None => (None, path),
            };
            if item == "self" {
                continue;
            }
            insert_pub_use(
                cap[1]
                    .split("::")
                    .chain(sub.into_iter().flat_map(|s| s.split("::"))),
                item,
                alias,
                file,
                containing_mod_path,
                reexports,
            );
        }
    }
}

/// Record one re-exported item: `pub use <prefix>::<item> [as <alias>]`.
fn insert_pub_use<'a>(
    prefix: impl Iterator<Item = &'a str>,
    item: &str,
    alias: Option<&str>,
    file: &Path,
    containing_mod_path: &[String],
    reexports: &mut ReExportMap,
) {
    let path_prefix: Vec<String> = prefix
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if path_prefix.is_empty() {
        return;
    }
    let exported_name = alias.unwrap_or(item).to_string();
    reexports.insert(
        (file.to_path_buf(), exported_name),
        ReExportEntry {
            path_prefix,
            original_name: item.to_string(),
            containing_mod_path: containing_mod_path.to_vec(),
        },
    );
}

/// Locate the child module file for `mod NAME;` declared in `parent_file`.
/// Convention mirrors the Rust reference:
/// - `src/lib.rs`, `src/main.rs`, `foo/mod.rs` and Cargo target roots
///   (`src/bin/x.rs`, `tests/x.rs`, ...) → children at `<dir>/NAME.rs`
///   or `<dir>/NAME/mod.rs`.
/// - `foo/bar.rs` → children at `foo/bar/NAME.rs` or `foo/bar/NAME/mod.rs`.
fn file_for_mod(parent_file: &Path, mod_name: &str) -> Option<PathBuf> {
    module_file_in(&rust_module_dir(parent_file)?, mod_name)
}

fn module_file_in(base: &Path, mod_name: &str) -> Option<PathBuf> {
    let flat = base.join(format!("{mod_name}.rs"));
    if flat.exists() {
        return Some(flat);
    }
    let mod_file = base.join(mod_name).join("mod.rs");
    if mod_file.exists() {
        return Some(mod_file);
    }
    None
}

// ---------------------------------------------------------------------------
// Cargo.toml parsing
// ---------------------------------------------------------------------------

fn parse_workspace_members(cargo_toml: &str, workspace_root: &Path) -> Vec<PathBuf> {
    let mut in_workspace = false;
    let mut in_members = false;
    let mut members: Vec<PathBuf> = Vec::new();
    let mut depth = 0i32;

    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if trimmed == "[workspace]" {
            in_workspace = true;
            in_members = false;
            continue;
        }
        if trimmed.starts_with('[') && !trimmed.starts_with("[[") {
            if in_workspace && !trimmed.starts_with("[workspace.") {
                in_workspace = false;
            }
            in_members = false;
            continue;
        }
        if !in_workspace {
            continue;
        }
        if trimmed.starts_with("members") && trimmed.contains('=') {
            in_members = true;
        }
        if in_members {
            for ch in trimmed.chars() {
                match ch {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth <= 0 {
                            in_members = false;
                        }
                    }
                    _ => {}
                }
            }
            // Extract quoted strings.
            let mut rest = trimmed;
            while let Some(start) = rest.find('"') {
                rest = &rest[start + 1..];
                if let Some(end) = rest.find('"') {
                    let pattern = &rest[..end];
                    rest = &rest[end + 1..];
                    // Expand glob (only `*` at the last level is supported
                    // by Cargo, e.g. `crates/*`).
                    if let Some((prefix, _)) = pattern.split_once('*') {
                        let base = workspace_root.join(prefix);
                        if let Ok(rd) = std::fs::read_dir(&base) {
                            let mut dirs: Vec<PathBuf> = rd
                                .flatten()
                                .filter(|e| e.path().is_dir())
                                .map(|e| e.path())
                                .collect();
                            dirs.sort();
                            for d in dirs {
                                if d.join("Cargo.toml").exists() {
                                    members.push(d);
                                }
                            }
                        }
                    } else {
                        let member = workspace_root.join(pattern);
                        if member.is_dir() {
                            members.push(member);
                        }
                    }
                } else {
                    break;
                }
            }
        }
    }
    members
}

/// The C2 rows [`RustWorkspaceModTree::build`] collects: every member the
/// module tree registers, in the order of its last listing.
#[derive(Default)]
struct CargoRows {
    members: Vec<CargoMember>,
}

/// One member [`RustWorkspaceModTree::build`] registers in `crates`.
struct CargoMember {
    /// The canonical member directory: two listings of it are one member.
    dir: PathBuf,
    /// `None` when the root or the lib entry does not relativise into the
    /// repo (`members = ["../x"]`): the table holds repo-relative paths, so
    /// the member has no row, yet it still shadows names as the index does.
    scope: Option<Scope>,
    /// The name code writes: the `[lib] name`, else the package name with
    /// `-` → `_`.
    code_name: String,
    /// The package-name spellings beside `code_name`, before `finish`
    /// settles which member claims each.
    spellings: Vec<String>,
    /// Position among the distinct members by first listing: `build` lets
    /// the first listing claim a spelling.
    first_listed: usize,
}

impl CargoRows {
    /// Record a member the module tree registers. `entries` are its target
    /// roots, the lib entry first. `crate_canon` is `crate_dir`
    /// canonicalized, `None` when that fails. A second listing of one
    /// directory moves the member to the end, as `build` lets the last
    /// listing keep the code name.
    fn add_member<'p>(
        &mut self,
        workspace_canon: &Path,
        crate_dir: &Path,
        crate_canon: Option<&Path>,
        under_workspace_manifest: bool,
        mut entries: impl Iterator<Item = &'p PathBuf>,
        manifest: &CrateManifest,
    ) {
        let dir = crate_canon.unwrap_or(crate_dir);
        if let Some(at) = self.members.iter().position(|member| member.dir == dir) {
            let member = self.members.remove(at);
            self.members.push(member);
            return;
        }
        let rel = |canon: &Path| {
            uid_path(canon, workspace_canon)
                .ok()
                .filter(|rel| !rel.contains('\\'))
        };
        let root = crate_canon.and_then(rel);
        let lib_entry = entries
            .next()
            .and_then(|entry| entry.canonicalize().ok())
            .and_then(|entry| rel(entry.as_path()));
        let scope = root.zip(lib_entry).map(|(root, lib_entry)| {
            let entry_files: Vec<String> = std::iter::once(lib_entry)
                .chain(entries.filter_map(|entry| rel(entry.canonicalize().ok()?.as_path())))
                .collect();
            let manifest_path = if root.is_empty() {
                "Cargo.toml".to_string()
            } else {
                format!("{root}/Cargo.toml")
            };
            let read = |path: String| ConfigRead {
                path,
                presence: ConfigPresence::Read,
            };
            let mut config_files = vec![read(manifest_path)];
            if under_workspace_manifest {
                config_files.push(read("Cargo.toml".to_string()));
            }
            Scope {
                ecosystem: Ecosystem::Cargo,
                root,
                config_files,
                membership: Membership::Subtree,
                entry_files,
                aliases: Vec::new(),
                search_roots: Vec::new(),
                dependencies: Vec::new(),
            }
        });
        let package_norm = manifest.name.replace('-', "_");
        let code_name = manifest
            .lib_name
            .clone()
            .unwrap_or_else(|| package_norm.clone());
        let mut spellings = Vec::new();
        for alias in [manifest.name.clone(), package_norm] {
            if alias != code_name && !spellings.contains(&alias) {
                spellings.push(alias);
            }
        }
        let first_listed = self.members.len();
        self.members.push(CargoMember {
            dir: dir.to_path_buf(),
            scope,
            code_name,
            spellings,
            first_listed,
        });
    }

    /// The table, binding what `build`'s `crates` map binds (C2 open point
    /// 2, option a). Of two members with one code name only the later
    /// listing keeps its `Primary` row, as the last `insert` wins. A
    /// package spelling goes to the first member by first listing that
    /// carries it, unless some member's code name is that spelling, as the
    /// `or_insert` fallback does. Both rules read every member, rows or not.
    fn finish(self) -> WorkspaceFacts {
        let members = self.members;
        let code_names: Vec<&str> = members.iter().map(|m| m.code_name.as_str()).collect();
        let mut by_first: Vec<usize> = (0..members.len()).collect();
        by_first.sort_by_key(|&i| members[i].first_listed);
        let mut taken: Vec<&str> = Vec::new();
        let mut alternates: Vec<Vec<String>> = vec![Vec::new(); members.len()];
        for i in by_first {
            for alias in &members[i].spellings {
                if !code_names.contains(&alias.as_str()) && !taken.contains(&alias.as_str()) {
                    taken.push(alias);
                    alternates[i].push(alias.clone());
                }
            }
        }
        let shadowed: Vec<bool> = (0..members.len())
            .map(|i| code_names[i + 1..].contains(&code_names[i]))
            .collect();
        let mut rows: Vec<(Scope, String, Vec<String>)> = members
            .into_iter()
            .zip(shadowed)
            .zip(alternates)
            .filter_map(|((member, shadowed), alternates)| {
                let primary = match shadowed {
                    true => String::new(),
                    false => member.code_name,
                };
                Some((member.scope?, primary, alternates))
            })
            .collect();
        rows.sort_by(|a, b| a.0.root.cmp(&b.0.root));
        let mut names: Vec<ScopeName> = Vec::new();
        for (id, (_, primary, alternates)) in rows.iter().enumerate() {
            let scope = ScopeId(id as u32);
            let ranked = std::iter::once((primary, NameRank::Primary))
                .chain(alternates.iter().map(|alias| (alias, NameRank::Alternate)));
            names.extend(
                ranked
                    .filter(|(name, _)| !name.trim().is_empty())
                    .map(|(name, rank)| ScopeName {
                        ecosystem: Ecosystem::Cargo,
                        name: name.clone(),
                        rank,
                        scope,
                    }),
            );
        }
        names.sort_by(|a, b| {
            (a.ecosystem, &a.name, a.rank, a.scope).cmp(&(b.ecosystem, &b.name, b.rank, b.scope))
        });
        names.dedup();
        let scopes = rows.into_iter().map(|(scope, ..)| scope).collect();
        // Every reader refuses a table that breaks C2's invariants, so store
        // none rather than rows nobody may read.
        WorkspaceFacts::new(scopes, names).unwrap_or_default()
    }
}

struct CrateManifest {
    name: String,
    lib_name: Option<String>,
    lib_path: Option<String>,
}

/// `[package] name`, `[lib] name` and `[lib] path` of a crate manifest.
fn read_crate_manifest(cargo_toml: &str) -> Option<CrateManifest> {
    let table: toml::Table = toml::from_str(cargo_toml).ok()?;
    let string_at =
        |section: &str, key: &str| table.get(section)?.get(key)?.as_str().map(str::to_string);
    Some(CrateManifest {
        name: string_at("package", "name")?,
        lib_name: string_at("lib", "name"),
        lib_path: string_at("lib", "path"),
    })
}

fn find_crate_entry(crate_dir: &Path, lib_path: Option<&str>) -> Option<PathBuf> {
    if let Some(lib_path) = lib_path {
        let p = crate_dir.join(lib_path);
        if p.exists() {
            return Some(p);
        }
    }
    let src = crate_dir.join("src");
    for name in ["lib.rs", "main.rs"] {
        let p = src.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// #[path = "..."] attribute collection
// ---------------------------------------------------------------------------

/// Scan cleaned source for `#[path = "rel"]` immediately before `mod NAME;`
/// and return a map `mod_name → rel_path`.
fn collect_path_attrs(clean: &str) -> FxHashMap<String, String> {
    let mut out: FxHashMap<String, String> = FxHashMap::default();
    let lines: Vec<&str> = clean.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if let Some(caps) = path_attr_re().captures(line) {
            let rel_path = caps[1].to_string();
            let lookahead_end = (i + 4).min(lines.len());
            for next_line in &lines[(i + 1)..lookahead_end] {
                if let Some(mcap) = mod_decl_re().captures(next_line) {
                    out.insert(mcap[1].to_string(), rel_path.clone());
                    break;
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::contracts::workspace::NameLookup;
    use std::fs;
    use tempfile::TempDir;

    fn make_tree(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, content) in files {
            let path = dir.path().join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, content).unwrap();
        }
        dir
    }

    #[test]
    fn test_file_for_mod_cargo_bin_root_finds_sibling_module() {
        let dir = make_tree(&[
            ("src/bin/tool.rs", "mod support;\n"),
            ("src/bin/support.rs", "pub fn f() {}\n"),
        ]);
        assert_eq!(
            file_for_mod(&dir.path().join("src/bin/tool.rs"), "support"),
            Some(dir.path().join("src/bin/support.rs"))
        );
    }

    #[test]
    fn test_file_for_mod_ordinary_file_finds_stem_directory_child() {
        let dir = make_tree(&[
            ("src/a/b.rs", "mod c;\n"),
            ("src/a/b/c.rs", "pub fn f() {}\n"),
            ("src/a/c.rs", "pub fn g() {}\n"),
        ]);
        assert_eq!(
            file_for_mod(&dir.path().join("src/a/b.rs"), "c"),
            Some(dir.path().join("src/a/b/c.rs"))
        );
    }

    // ── `[lib] name` / `[lib] path` overrides ──────────────────────────────

    /// The bin and every dependent name a crate by its lib target, so a
    /// `[lib] name` that differs from the package name is the only spelling
    /// real code uses (`egent-code-plexus` is imported as `ecp_cli`).
    #[test]
    fn lib_name_override_resolves_as_crate_name() {
        let dir = make_tree(&[
            (
                "Cargo.toml",
                "[package]\nname = \"my-app\"\n\n[lib]\nname = \"app_core\"\npath = \"src/lib.rs\"\n",
            ),
            ("src/lib.rs", "pub mod foo;\n"),
            ("src/foo.rs", "pub fn bar() {}\n"),
            ("src/main.rs", "fn main() { app_core::foo::bar(); }\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let r = tree
            .resolve_fqn("app_core::foo::bar", "src/main.rs", dir.path())
            .expect("lib name resolves");
        assert!(r.file.ends_with("foo.rs"), "got {}", r.file);
        assert_eq!(r.item_name, "bar");
    }

    #[test]
    fn lib_name_override_resolves_in_workspace_member() {
        let dir = make_tree(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/app\"]\n"),
            (
                "crates/app/Cargo.toml",
                "[package]\nname = \"my-app\"\n\n[[bin]]\nname = \"app\"\npath = \"src/main.rs\"\n\n[lib]\nname = \"app_core\"\n",
            ),
            ("crates/app/src/lib.rs", "pub mod foo;\n"),
            ("crates/app/src/foo.rs", "pub fn bar() {}\n"),
            ("crates/app/src/main.rs", "fn main() {}\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let r = tree
            .resolve_fqn("app_core::foo::bar", "crates/app/src/main.rs", dir.path())
            .expect("lib name resolves in a member crate");
        assert!(r.file.ends_with("foo.rs"), "got {}", r.file);
        assert!(
            tree.resolve_fqn("my_app::foo::bar", "crates/app/src/main.rs", dir.path())
                .is_some(),
            "the package-name spelling keeps resolving"
        );
    }

    #[test]
    fn lib_override_with_trailing_comments_resolves() {
        let dir = make_tree(&[
            (
                "Cargo.toml",
                "[package]\nname = \"my-app\" # pkg\n\n[lib] # target\nname = \"app_core\" # code name\npath = \"src/core.rs\" # entry\n",
            ),
            ("src/core.rs", "pub mod foo;\n"),
            ("src/foo.rs", "pub fn bar() {}\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let r = tree
            .resolve_fqn("app_core::foo::bar", "src/core.rs", dir.path())
            .expect("commented [lib] values resolve");
        assert!(r.file.ends_with("foo.rs"), "got {}", r.file);
    }

    /// Code names `alpha` as `shared` (its lib name); a package that happens
    /// to be called `shared` must not take that key.
    #[test]
    fn lib_name_wins_over_another_crates_package_name() {
        for members in [
            "[\"crates/a\", \"crates/b\"]",
            "[\"crates/b\", \"crates/a\"]",
        ] {
            let dir = make_tree(&[
                ("Cargo.toml", &format!("[workspace]\nmembers = {members}\n")),
                (
                    "crates/a/Cargo.toml",
                    "[package]\nname = \"alpha\"\n\n[lib]\nname = \"shared\"\n",
                ),
                ("crates/a/src/lib.rs", "pub mod registry;\n"),
                ("crates/a/src/registry.rs", "pub fn lookup() {}\n"),
                (
                    "crates/b/Cargo.toml",
                    "[package]\nname = \"shared\"\n\n[lib]\nname = \"backend\"\n",
                ),
                ("crates/b/src/lib.rs", "pub mod registry;\n"),
                ("crates/b/src/registry.rs", "pub fn lookup() {}\n"),
            ]);
            let tree = RustWorkspaceModTree::build(dir.path());
            let r = tree
                .resolve_fqn(
                    "shared::registry::lookup",
                    "crates/a/src/lib.rs",
                    dir.path(),
                )
                .expect("resolves");
            assert!(
                r.file.ends_with("crates/a/src/registry.rs"),
                "{members}: got {}",
                r.file
            );
            let own = tree
                .resolve_fqn("crate::registry::lookup", "crates/b/src/lib.rs", dir.path())
                .expect("crate:: in b resolves");
            assert!(
                own.file.ends_with("crates/b/src/registry.rs"),
                "{members}: got {}",
                own.file
            );
        }
    }

    #[test]
    fn lib_path_override_is_the_crate_entry() {
        let dir = make_tree(&[
            (
                "Cargo.toml",
                "[package]\nname = \"mycrate\"\n\n[lib]\npath = \"src/core.rs\"\n",
            ),
            ("src/core.rs", "pub mod foo;\n"),
            ("src/foo.rs", "pub fn bar() {}\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let r = tree
            .resolve_fqn("crate::foo::bar", "src/core.rs", dir.path())
            .expect("lib path is the module-tree root");
        assert!(r.file.ends_with("foo.rs"), "got {}", r.file);
    }

    // ── 2-segment `mod::fn` (regression for PR #75's case) ─────────────────

    #[test]
    fn two_segment_mod_fn() {
        let dir = make_tree(&[
            ("Cargo.toml", "[package]\nname = \"mycrate\"\n"),
            ("src/lib.rs", "pub mod foo;\n"),
            ("src/foo.rs", "pub fn bar() {}\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let r = tree.resolve_fqn("crate::foo::bar", "src/lib.rs", dir.path());
        assert!(r.is_some(), "2-seg should resolve");
        let r = r.unwrap();
        assert!(
            r.file.ends_with("foo.rs"),
            "expected foo.rs, got {}",
            r.file
        );
        assert_eq!(r.item_name, "bar");
    }

    // ── 3-segment `crate::a::b::fn` ────────────────────────────────────────

    #[test]
    fn three_segment_crate_a_b_fn() {
        let dir = make_tree(&[
            ("Cargo.toml", "[package]\nname = \"mycrate\"\n"),
            ("src/lib.rs", "pub mod a;\n"),
            ("src/a.rs", "pub mod b;\n"),
            ("src/a/b.rs", "pub fn func() {}\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let r = tree.resolve_fqn("crate::a::b::func", "src/lib.rs", dir.path());
        assert!(r.is_some(), "3-seg should resolve");
        let r = r.unwrap();
        assert!(r.file.ends_with("b.rs"), "expected b.rs, got {}", r.file);
        assert_eq!(r.item_name, "func");
    }

    // ── 4-segment `crate::a::b::c::fn` ─────────────────────────────────────

    #[test]
    fn four_segment_crate_a_b_c_fn() {
        let dir = make_tree(&[
            ("Cargo.toml", "[package]\nname = \"mycrate\"\n"),
            ("src/lib.rs", "pub mod a;\n"),
            ("src/a.rs", "pub mod b;\n"),
            ("src/a/b.rs", "pub mod c;\n"),
            ("src/a/b/c.rs", "pub fn deep() {}\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let r = tree.resolve_fqn("crate::a::b::c::deep", "src/lib.rs", dir.path());
        assert!(r.is_some(), "4-seg should resolve");
        let r = r.unwrap();
        assert!(r.file.ends_with("c.rs"), "expected c.rs, got {}", r.file);
        assert_eq!(r.item_name, "deep");
    }

    // ── `super::fn` ─────────────────────────────────────────────────────────

    #[test]
    fn super_fn() {
        let dir = make_tree(&[
            ("Cargo.toml", "[package]\nname = \"mycrate\"\n"),
            ("src/lib.rs", "pub mod child;\npub fn parent_fn() {}\n"),
            ("src/child.rs", "pub fn caller() { super::parent_fn(); }\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        // `super::parent_fn` from src/child.rs → caller is in mod `child`,
        // super → mod `[]` (root), which is src/lib.rs.
        let r = tree.resolve_fqn("super::parent_fn", "src/child.rs", dir.path());
        assert!(r.is_some(), "super::fn should resolve, got {:?}", r);
        let r = r.unwrap();
        assert!(
            r.file.ends_with("lib.rs"),
            "expected lib.rs, got {}",
            r.file
        );
        assert_eq!(r.item_name, "parent_fn");
    }

    // ── `super::super::fn` ──────────────────────────────────────────────────

    #[test]
    fn super_super_fn() {
        let dir = make_tree(&[
            ("Cargo.toml", "[package]\nname = \"mycrate\"\n"),
            ("src/lib.rs", "pub mod a;\npub fn root_fn() {}\n"),
            ("src/a.rs", "pub mod b;\n"),
            ("src/a/b.rs", "fn caller() { super::super::root_fn(); }\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let r = tree.resolve_fqn("super::super::root_fn", "src/a/b.rs", dir.path());
        assert!(r.is_some(), "super::super::fn should resolve");
        let r = r.unwrap();
        assert!(
            r.file.ends_with("lib.rs"),
            "expected lib.rs, got {}",
            r.file
        );
    }

    // ── `self::fn` ──────────────────────────────────────────────────────────

    #[test]
    fn self_fn() {
        let dir = make_tree(&[
            ("Cargo.toml", "[package]\nname = \"mycrate\"\n"),
            ("src/lib.rs", "pub mod foo;\n"),
            (
                "src/foo.rs",
                "pub fn helper() {}\nfn caller() { self::helper(); }\n",
            ),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let r = tree.resolve_fqn("self::helper", "src/foo.rs", dir.path());
        assert!(r.is_some(), "self::fn should resolve");
        let r = r.unwrap();
        assert!(
            r.file.ends_with("foo.rs"),
            "expected foo.rs, got {}",
            r.file
        );
        assert_eq!(r.item_name, "helper");
    }

    // ── `#[path = "..."]` redirect ──────────────────────────────────────────

    #[test]
    fn path_attribute_redirect() {
        let dir = make_tree(&[
            ("Cargo.toml", "[package]\nname = \"mycrate\"\n"),
            (
                "src/lib.rs",
                "#[path = \"impl/real.rs\"]\npub mod actual;\n",
            ),
            ("src/impl/real.rs", "pub fn redirected() {}\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let r = tree.resolve_fqn("crate::actual::redirected", "src/lib.rs", dir.path());
        assert!(r.is_some(), "#[path] redirect should resolve, got {:?}", r);
        let r = r.unwrap();
        assert!(
            r.file.ends_with("real.rs"),
            "expected real.rs, got {}",
            r.file
        );
    }

    // ── Inline `mod foo { ... }` boundary ───────────────────────────────────

    #[test]
    fn inline_mod_does_not_follow_file() {
        // Inline `mod foo { fn bar() {} }` — no file is generated, so
        // `crate::foo::bar` won't resolve to a separate file. The resolver
        // correctly returns None because no mod_path entry exists for `foo`.
        let dir = make_tree(&[
            ("Cargo.toml", "[package]\nname = \"mycrate\"\n"),
            ("src/lib.rs", "pub mod foo { pub fn bar() {} }\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        // The inline mod has no backing file, so resolution should fall back.
        // We don't assert a value here — just ensure no panic.
        let _r = tree.resolve_fqn("crate::foo::bar", "src/lib.rs", dir.path());
        // Expected: None (inline mods are out of scope), or Some pointing to lib.rs.
        // Both are acceptable; the test pins "no crash".
    }

    // ── External std path does not resolve ──────────────────────────────────

    #[test]
    fn external_std_does_not_resolve() {
        let dir = make_tree(&[
            ("Cargo.toml", "[package]\nname = \"mycrate\"\n"),
            ("src/lib.rs", ""),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let r = tree.resolve_fqn("std::collections::HashMap::new", "src/lib.rs", dir.path());
        assert!(r.is_none(), "std path must not resolve to a file");
    }

    // ── mod/mod.rs style ────────────────────────────────────────────────────

    #[test]
    fn mod_rs_style() {
        let dir = make_tree(&[
            ("Cargo.toml", "[package]\nname = \"mycrate\"\n"),
            ("src/lib.rs", "pub mod subdir;\n"),
            ("src/subdir/mod.rs", "pub fn inside() {}\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let r = tree.resolve_fqn("crate::subdir::inside", "src/lib.rs", dir.path());
        assert!(r.is_some(), "mod/mod.rs style should resolve");
        let r = r.unwrap();
        assert!(
            r.file.ends_with("mod.rs"),
            "expected mod.rs, got {}",
            r.file
        );
    }

    #[test]
    fn test_anchored_module_file_follows_logical_modules_and_bin_roots() {
        let dir = make_tree(&[
            ("Cargo.toml", "[package]\nname = \"tool\"\n"),
            ("src/lib.rs", "pub mod a;\n"),
            ("src/a.rs", "#[path = \"deep/b_impl.rs\"]\nmod b;\n"),
            ("src/deep/b_impl.rs", "pub fn f() {}\n"),
            ("src/main.rs", "mod cli;\nfn main() {}\n"),
            ("src/cli.rs", "pub fn g() {}\n"),
            ("src/bin/tool/main.rs", "mod support;\nfn main() {}\n"),
            ("src/bin/tool/support.rs", "pub fn h() {}\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let at = |caller: &str, path: &str| tree.anchored_module_file(caller, path);
        assert_eq!(at("src/deep/b_impl.rs", "super"), Some("src/a.rs"));
        assert_eq!(at("src/deep/b_impl.rs", "super::super"), Some("src/lib.rs"));
        assert_eq!(at("src/deep/b_impl.rs", "self"), Some("src/deep/b_impl.rs"));
        assert_eq!(at("src/a.rs", "crate"), Some("src/lib.rs"));
        assert_eq!(at("src/a.rs", "self::b"), Some("src/deep/b_impl.rs"));
        assert_eq!(at("src/cli.rs", "crate"), Some("src/main.rs"));
        assert_eq!(
            at("src/bin/tool/support.rs", "crate"),
            Some("src/bin/tool/main.rs")
        );
    }

    #[test]
    fn test_anchored_module_file_unplaced_paths_are_none() {
        let dir = make_tree(&[
            ("Cargo.toml", "[package]\nname = \"tool\"\n"),
            ("src/lib.rs", "pub mod a;\npub mod cli;\n"),
            ("src/a.rs", "pub fn f() {}\n"),
            ("src/main.rs", "mod cli;\nfn main() {}\n"),
            ("src/cli.rs", "pub fn g() {}\n"),
            ("src/orphan.rs", "pub fn o() {}\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let at = |caller: &str, path: &str| tree.anchored_module_file(caller, path);
        assert_eq!(at("src/a.rs", "super::super"), None, "above the root");
        assert_eq!(at("src/a.rs", "::"), None, "only separators");
        assert_eq!(at("src/a.rs", ""), None, "empty");
        assert_eq!(at("src/a.rs", "std::fs"), None, "unanchored");
        assert_eq!(at("src/a.rs", "crate::missing"), None, "undeclared module");
        assert_eq!(at("src/orphan.rs", "crate"), None, "file outside the tree");
        assert_eq!(
            at("src/cli.rs", "crate"),
            None,
            "lib and bin both declare it"
        );
        assert_eq!(at("src/cli.rs", "super"), None, "two roots disagree");
    }

    #[test]
    fn test_anchored_module_file_without_manifest_is_none() {
        let dir = make_tree(&[("src/lib.rs", "pub mod a;\n"), ("src/a.rs", "")]);
        let tree = RustWorkspaceModTree::build(dir.path());
        assert_eq!(tree.anchored_module_file("src/a.rs", "crate"), None);
    }

    /// `(name, rank, scope root)` of every name row, in table order.
    fn name_rows(facts: &WorkspaceFacts) -> Vec<(&str, NameRank, &str)> {
        facts
            .names
            .iter()
            .map(|entry| {
                (
                    entry.name.as_str(),
                    entry.rank,
                    facts.scopes[entry.scope.index()].root.as_str(),
                )
            })
            .collect()
    }

    fn config_paths(scope: &Scope) -> Vec<&str> {
        scope
            .config_files
            .iter()
            .map(|config| {
                assert_eq!(config.presence, ConfigPresence::Read, "{}", config.path);
                config.path.as_str()
            })
            .collect()
    }

    #[test]
    fn test_workspace_facts_members_mirror_crates_map_keys() {
        let dir = make_tree(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/b\", \"crates/a\", \"crates/b\", \"crates/nolib\"]\n",
            ),
            ("crates/a/Cargo.toml", "[package]\nname = \"a-core\"\n"),
            ("crates/a/src/lib.rs", ""),
            ("crates/a/src/main.rs", "fn main() {}\n"),
            ("crates/a/src/bin/tool.rs", "fn main() {}\n"),
            (
                "crates/b/Cargo.toml",
                "[package]\nname = \"b\"\n\n[lib]\nname = \"other\"\n",
            ),
            ("crates/b/src/lib.rs", ""),
            ("crates/nolib/Cargo.toml", "[package]\nname = \"nolib\"\n"),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let facts = tree.workspace_facts();
        assert_eq!(facts.validate(), Ok(()));
        let scopes: Vec<(&str, Vec<&str>, Vec<&str>)> = facts
            .scopes
            .iter()
            .map(|scope| {
                assert_eq!(scope.ecosystem, Ecosystem::Cargo);
                assert_eq!(scope.membership, Membership::Subtree);
                (
                    scope.root.as_str(),
                    config_paths(scope),
                    scope.entry_files.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            scopes,
            vec![
                (
                    "crates/a",
                    vec!["crates/a/Cargo.toml", "Cargo.toml"],
                    vec![
                        "crates/a/src/lib.rs",
                        "crates/a/src/main.rs",
                        "crates/a/src/bin/tool.rs",
                    ],
                ),
                (
                    "crates/b",
                    vec!["crates/b/Cargo.toml", "Cargo.toml"],
                    vec!["crates/b/src/lib.rs"],
                ),
            ],
            "sorted by root; the repeated member once; the member without a target none"
        );
        assert_eq!(
            name_rows(facts),
            vec![
                ("a-core", NameRank::Alternate, "crates/a"),
                ("a_core", NameRank::Primary, "crates/a"),
                ("b", NameRank::Alternate, "crates/b"),
                ("other", NameRank::Primary, "crates/b"),
            ]
        );
        // Every name the table answers is a key the index resolves through.
        for name in ["a-core", "a_core", "b", "other"] {
            assert!(tree.crates.contains_key(name), "{name}");
        }
        assert_eq!(
            facts.lookup_name(Ecosystem::Cargo, "nolib"),
            NameLookup::Missing
        );
    }

    #[test]
    fn test_workspace_facts_single_package_root_scope_is_repo_root() {
        let dir = make_tree(&[
            ("Cargo.toml", "[package]\nname = \"tool\"\n"),
            ("src/lib.rs", ""),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let facts = tree.workspace_facts();
        assert_eq!(facts.validate(), Ok(()));
        assert_eq!(facts.scopes.len(), 1);
        assert_eq!(facts.scopes[0].root, "");
        assert_eq!(config_paths(&facts.scopes[0]), vec!["Cargo.toml"]);
        assert_eq!(facts.scopes[0].entry_files, vec!["src/lib.rs".to_string()]);
        assert_eq!(name_rows(facts), vec![("tool", NameRank::Primary, "")]);
    }

    #[test]
    fn test_workspace_facts_duplicate_code_name_last_member_keeps_row() {
        let dir = make_tree(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"x1\", \"x2\"]\n"),
            ("x1/Cargo.toml", "[package]\nname = \"dup\"\n"),
            ("x1/src/lib.rs", ""),
            ("x2/Cargo.toml", "[package]\nname = \"dup\"\n"),
            ("x2/src/lib.rs", ""),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        let facts = tree.workspace_facts();
        assert_eq!(name_rows(facts), vec![("dup", NameRank::Primary, "x2")]);
        assert_eq!(
            facts.lookup_name(Ecosystem::Cargo, "dup"),
            NameLookup::Unique(ScopeId(1))
        );
    }

    #[test]
    fn test_workspace_facts_relisted_member_last_listing_keeps_row() {
        let dir = make_tree(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"x1\", \"x2\", \"x1\"]\n",
            ),
            ("x1/Cargo.toml", "[package]\nname = \"dup\"\n"),
            ("x1/src/lib.rs", ""),
            ("x2/Cargo.toml", "[package]\nname = \"dup\"\n"),
            ("x2/src/lib.rs", ""),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        assert_eq!(
            tree.crates["dup"].0,
            dir.path().join("x1"),
            "the index binds the last listing"
        );
        let facts = tree.workspace_facts();
        assert_eq!(facts.validate(), Ok(()));
        assert_eq!(name_rows(facts), vec![("dup", NameRank::Primary, "x1")]);
        assert_eq!(
            facts.lookup_name(Ecosystem::Cargo, "dup"),
            NameLookup::Unique(ScopeId(0))
        );
    }

    #[test]
    fn test_workspace_facts_relisted_member_first_listing_keeps_spelling() {
        let dir = make_tree(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"x1\", \"x2\", \"x1\"]\n",
            ),
            (
                "x1/Cargo.toml",
                "[package]\nname = \"pkg\"\n\n[lib]\nname = \"l1\"\n",
            ),
            ("x1/src/lib.rs", ""),
            (
                "x2/Cargo.toml",
                "[package]\nname = \"pkg\"\n\n[lib]\nname = \"l2\"\n",
            ),
            ("x2/src/lib.rs", ""),
        ]);
        let tree = RustWorkspaceModTree::build(dir.path());
        assert_eq!(tree.crates["pkg"].0, dir.path().join("x1"));
        assert_eq!(
            name_rows(tree.workspace_facts()),
            vec![
                ("l1", NameRank::Primary, "x1"),
                ("l2", NameRank::Primary, "x2"),
                ("pkg", NameRank::Alternate, "x1"),
            ],
            "the package spelling stays with the first listing, as in the index"
        );
    }

    #[test]
    fn test_workspace_facts_outside_member_shadows_inside_primary() {
        let dir = make_tree(&[
            (
                "ws/Cargo.toml",
                "[workspace]\nmembers = [\"a\", \"../ext\"]\n",
            ),
            (
                "ws/a/Cargo.toml",
                "[package]\nname = \"a\"\n\n[lib]\nname = \"util\"\n",
            ),
            ("ws/a/src/lib.rs", ""),
            (
                "ext/Cargo.toml",
                "[package]\nname = \"ext\"\n\n[lib]\nname = \"util\"\n",
            ),
            ("ext/src/lib.rs", ""),
        ]);
        let ws = dir.path().join("ws");
        let tree = RustWorkspaceModTree::build(&ws);
        assert_eq!(
            tree.crates["util"].0,
            ws.join("../ext"),
            "the index binds `util` to the out-of-repo member"
        );
        let facts = tree.workspace_facts();
        assert_eq!(facts.validate(), Ok(()));
        assert_eq!(name_rows(facts), vec![("a", NameRank::Alternate, "a")]);
        assert_eq!(
            facts.lookup_name(Ecosystem::Cargo, "util"),
            NameLookup::Missing,
            "the overlay must not bind `util` to `a`"
        );
    }

    #[test]
    fn test_workspace_facts_outside_code_name_blocks_inside_spelling() {
        let dir = make_tree(&[
            (
                "ws/Cargo.toml",
                "[workspace]\nmembers = [\"a\", \"../ext\"]\n",
            ),
            (
                "ws/a/Cargo.toml",
                "[package]\nname = \"util\"\n\n[lib]\nname = \"a_lib\"\n",
            ),
            ("ws/a/src/lib.rs", ""),
            (
                "ext/Cargo.toml",
                "[package]\nname = \"ext\"\n\n[lib]\nname = \"util\"\n",
            ),
            ("ext/src/lib.rs", ""),
        ]);
        let ws = dir.path().join("ws");
        let tree = RustWorkspaceModTree::build(&ws);
        assert_eq!(tree.crates["util"].0, ws.join("../ext"));
        let facts = tree.workspace_facts();
        assert_eq!(name_rows(facts), vec![("a_lib", NameRank::Primary, "a")]);
        assert_eq!(
            facts.lookup_name(Ecosystem::Cargo, "util"),
            NameLookup::Missing,
            "a code name outranks a package spelling in the index, row or not"
        );
    }

    #[test]
    fn test_workspace_facts_outside_member_and_blank_name_get_no_row() {
        let dir = make_tree(&[
            (
                "ws/Cargo.toml",
                "[workspace]\nmembers = [\"../ext\", \"blank\", \"ok\"]\n",
            ),
            ("ext/Cargo.toml", "[package]\nname = \"ext\"\n"),
            ("ext/src/lib.rs", ""),
            ("ws/blank/Cargo.toml", "[package]\nname = \" \"\n"),
            ("ws/blank/src/lib.rs", ""),
            ("ws/ok/Cargo.toml", "[package]\nname = \"ok\"\n"),
            ("ws/ok/src/lib.rs", ""),
        ]);
        let tree = RustWorkspaceModTree::build(&dir.path().join("ws"));
        let facts = tree.workspace_facts();
        assert_eq!(facts.validate(), Ok(()));
        let roots: Vec<&str> = facts.scopes.iter().map(|s| s.root.as_str()).collect();
        assert_eq!(
            roots,
            vec!["blank", "ok"],
            "`../ext` has no repo-relative root"
        );
        assert_eq!(name_rows(facts), vec![("ok", NameRank::Primary, "ok")]);
    }

    #[test]
    fn test_workspace_facts_without_manifest_is_empty() {
        let dir = make_tree(&[("src/lib.rs", ""), ("pkg/m.py", "")]);
        let tree = RustWorkspaceModTree::build(dir.path());
        assert_eq!(tree.workspace_facts(), &WorkspaceFacts::default());
    }
}
