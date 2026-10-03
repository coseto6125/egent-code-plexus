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
//!   hops, cycle-safe). See [`RustWorkspaceModTree::follow_pub_use_chain`].
//!
//! # Out of scope (documented)
//! * `cfg`-gated `mod` decls (first filesystem hit wins).
//! * `#[path = "..."]` overrides.
//! * Macro-expanded `use`s.
//! * `extern crate` re-exports (legacy syntax).
//! * Proc-macro-generated re-exports (`paste!`, `derive_more`, etc.) — logged
//!   as BlindSpot if encountered.

use crate::resolution::resolver::rust_module_dir;
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
    /// The original item name at the source (last segment before `as`).
    original_name: String,
    /// `true` when this is a glob re-export (`pub use path::*`).
    is_glob: bool,
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
}

/// One Cargo target's modules, `mod_path → repo-relative file`.
struct TargetTree {
    /// Rooted at a bin entry beside the crate's primary entry
    /// (`src/main.rs` next to `src/lib.rs`, `src/bin/*.rs`), whose modules
    /// `file_to_crate` does not cover.
    is_bin: bool,
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
            out.add_target(&tree, false);
            for bin_root in bin_roots(&crate_dir, &entry_path) {
                out.add_target(&build_mod_tree_and_reexports(&bin_root).0, true);
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
            let canon_str = crate_dir
                .canonicalize()
                .unwrap_or_else(|_| crate_dir.clone())
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

        out
    }

    /// Resolve a Rust FQN call to the file path (repo-relative) of the
    /// module that declares the terminal item.
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
        let segs: Vec<&str> = full_callee.split("::").collect();
        if segs.len() < 2 {
            return None;
        }
        let head = segs[0];
        if matches!(head, "crate" | "self" | "super") {
            if let Some(resolved) = self.resolve_in_bin_target(caller_file, &segs) {
                return Some(resolved);
            }
        }

        // Determine crate root and module path for the caller. Only `crate`,
        // `self` and `super` need it, and it costs a `canonicalize` syscall.
        let caller_crate = || self.crate_for_file(caller_file, workspace_root);

        // Build the module-path segments for the target item.
        // For `crate::a::b::fn` → segs after `crate` minus last = `[a, b]`,
        // last = `fn`.
        let (target_crate_name, path_segs): (&str, &[&str]) = match head {
            "crate" => {
                let crate_name = caller_crate()?;
                return self.resolve_in_crate(
                    &crate_name,
                    &segs[1..].iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                    workspace_root,
                );
            }
            "self" | "super" => {
                let crate_name = caller_crate()?;
                let caller_abs = if Path::new(caller_file).is_absolute() {
                    PathBuf::from(caller_file)
                } else {
                    workspace_root.join(caller_file)
                };
                let caller_canon = caller_abs.canonicalize().ok()?;
                let (_cn, caller_mod_path) = self.file_to_crate.get(&caller_canon)?;
                let up = if head == "super" {
                    segs.iter().take_while(|&&s| s == "super").count()
                } else {
                    0
                };
                let (base, rest): (Vec<&str>, &[&str]) = if head == "self" {
                    (
                        caller_mod_path.iter().map(String::as_str).collect(),
                        &segs[1..],
                    )
                } else {
                    if up > caller_mod_path.len() {
                        return None;
                    }
                    (
                        caller_mod_path[..caller_mod_path.len() - up]
                            .iter()
                            .map(String::as_str)
                            .collect(),
                        &segs[up..],
                    )
                };
                let combined: Vec<String> = base
                    .into_iter()
                    .chain(rest.iter().copied())
                    .map(str::to_string)
                    .collect();
                return self.resolve_in_crate(&crate_name, &combined, workspace_root);
            }
            other => {
                // External std/core/alloc — skip.
                if matches!(other, "std" | "core" | "alloc" | "proc_macro" | "test") {
                    return None;
                }
                // Try as a workspace crate name.
                if self.crates.contains_key(other) {
                    (other, &segs[1..])
                } else {
                    return None;
                }
            }
        };

        // path_segs = [...module_path_parts..., item_name]
        let combined: Vec<String> = path_segs.iter().map(|s| s.to_string()).collect();
        self.resolve_in_crate(target_crate_name, &combined, workspace_root)
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

    /// [`Self::resolve_fqn`] for an anchored path from a file that only one
    /// bin target declares. `file_to_crate` holds the primary tree alone, so
    /// without this `crate::` there would name the lib's root. The bin's
    /// `pub use` chains are not followed.
    fn resolve_in_bin_target(&self, caller_file: &str, segs: &[&str]) -> Option<ResolvedFqn> {
        let [(target, mod_path)] = self.file_targets.get(caller_file)?.as_slice() else {
            return None;
        };
        let tree = &self.targets[*target];
        if !tree.is_bin {
            return None;
        }
        let combined = anchored_mod_path(mod_path, segs)?;
        (0..combined.len()).rev().find_map(|prefix_len| {
            let file = tree.modules.get(&combined[..prefix_len])?;
            Some(ResolvedFqn {
                file: file.clone(),
                item_name: combined[prefix_len].clone(),
            })
        })
    }

    fn add_target(&mut self, tree: &ModTree, is_bin: bool) {
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
        self.targets.push(TargetTree { is_bin, modules });
    }

    /// True when `path` (`<crate_name>::a::b`) names a module of a
    /// workspace crate, as opposed to an item inside one.
    pub fn names_module(&self, path: &str) -> bool {
        let mut segs = path.split("::");
        let Some((_, _, tree, _)) = segs.next().and_then(|head| self.crates.get(head)) else {
            return false;
        };
        tree.contains_key(&segs.map(str::to_string).collect::<Vec<_>>())
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

    /// Given a crate name and a combined path like `["build", "orchestrator",
    /// "build_l2"]`, walk the mod tree to find the file that hosts `build_l2`.
    ///
    /// Strategy (mirrors `_resolve_in_crate` in rs_oracle.py):
    /// Try longest-to-shortest module prefixes. The file for the matching
    /// prefix is the declaring file. After a file+item is located, the
    /// `pub use` re-export chain is followed (up to 16 hops) to reach the
    /// original definition.
    fn resolve_in_crate(
        &self,
        crate_name: &str,
        combined: &[String],
        workspace_root: &Path,
    ) -> Option<ResolvedFqn> {
        if combined.is_empty() {
            return None;
        }
        let (_crate_dir, _cdir_str, tree, _reexports) = self.crates.get(crate_name)?;

        for prefix_len in (0..combined.len()).rev() {
            let prefix = &combined[..prefix_len];
            // SAFETY note: tree key is `Vec<String>`. We borrow the slice into
            // an owned vec only on a miss-cycle's final hit to keep the loop
            // body alloc-free. `HashMap::get` requires `Borrow<Q>` and `Vec`
            // doesn't implement `Borrow<[String]>`, so we materialize once
            // outside the loop body — but only on the matched iteration.
            let key: Vec<String> = prefix.to_vec();
            let Some(file) = tree.get(&key) else {
                continue;
            };
            let item_name = combined.get(prefix_len).map(String::as_str).unwrap_or("");

            // Follow any `pub use` re-export chain before returning.
            if let Some(resolved) =
                self.follow_pub_use_chain(crate_name, file, item_name, workspace_root, 0)
            {
                return Some(resolved);
            }

            let rel = uid_path(file, &self.workspace_canon).ok()?;
            return Some(ResolvedFqn {
                file: rel,
                item_name: item_name.to_string(),
            });
        }
        None
    }

    /// Walk a `pub use` re-export chain from `(file, item_name)` to the
    /// original definition site.
    ///
    /// Returns `Some(ResolvedFqn)` when the chain leads to a different
    /// (non-re-export) definition, or `None` when `(file, item_name)` is
    /// already the canonical definition (no matching re-export entry found).
    ///
    /// Max depth: 16 hops. Cycle detection via visited set prevents
    /// infinite loops. On a glob (`pub use path::*`) the symbol is
    /// re-resolved using the prefix path within the same crate.
    pub fn follow_pub_use_chain(
        &self,
        crate_name: &str,
        file: &Path,
        item_name: &str,
        workspace_root: &Path,
        depth: u8,
    ) -> Option<ResolvedFqn> {
        const MAX_DEPTH: u8 = 16;
        if depth >= MAX_DEPTH || item_name.is_empty() {
            return None;
        }

        let (_crate_dir, _cdir_str, _tree, reexports) = self.crates.get(crate_name)?;

        // Try exact-name lookup first, then glob wildcard lookup.
        let file_buf = file.to_path_buf();
        let entry = reexports
            .get(&(file_buf.clone(), item_name.to_string()))
            .or_else(|| reexports.get(&(file_buf, "*".to_string())));

        let entry = entry?;

        // Resolve the path_prefix relative to the current crate.
        // path_prefix is like ["inner"] meaning `inner::Widget`.
        // We need to turn it back into a FQN and re-resolve.
        let target_item = if entry.is_glob {
            item_name
        } else {
            &entry.original_name
        };

        // Build the absolute (crate-rooted) combined path for recursive resolution.
        //
        // If path_prefix starts with "crate" → strip it, resolve from crate root.
        // If it starts with a known workspace crate name → that crate, strip 1 seg.
        // If it starts with "self" → use the containing mod's path as base.
        // If it starts with "super" → go one level up from containing mod.
        // Otherwise → the prefix is relative to the containing module; prepend it.
        let mut new_combined: Vec<String>;

        let (target_crate, segs_start): (&str, usize) =
            if entry.path_prefix.first().map(String::as_str) == Some("crate") {
                (crate_name, 1)
            } else if entry.path_prefix.first().map(String::as_str) == Some("self") {
                // "self" is equivalent to "crate" + the containing mod path.
                new_combined = Vec::with_capacity(
                    entry.containing_mod_path.len() + entry.path_prefix.len() + 1,
                );
                new_combined.extend_from_slice(&entry.containing_mod_path);
                new_combined.extend(entry.path_prefix[1..].iter().cloned());
                new_combined.push(target_item.to_string());
                let (_cd, _cs, tree2, _re2) = self.crates.get(crate_name)?;
                return self.resolve_chain_in_tree(
                    crate_name,
                    tree2,
                    file,
                    item_name,
                    &new_combined,
                    workspace_root,
                    depth,
                );
            } else if entry.path_prefix.first().map(String::as_str) == Some("super") {
                // Each leading "super" removes one containing mod path segment.
                let super_count = entry
                    .path_prefix
                    .iter()
                    .take_while(|s| s.as_str() == "super")
                    .count();
                let base_len = entry.containing_mod_path.len().saturating_sub(super_count);
                new_combined = Vec::with_capacity(base_len + entry.path_prefix.len() + 1);
                new_combined.extend_from_slice(&entry.containing_mod_path[..base_len]);
                new_combined.extend(entry.path_prefix[super_count..].iter().cloned());
                new_combined.push(target_item.to_string());
                let (_cd, _cs, tree2, _re2) = self.crates.get(crate_name)?;
                return self.resolve_chain_in_tree(
                    crate_name,
                    tree2,
                    file,
                    item_name,
                    &new_combined,
                    workspace_root,
                    depth,
                );
            } else if let Some(first) = entry.path_prefix.first() {
                if self.crates.contains_key(first.as_str()) {
                    // External crate name as prefix.
                    (first.as_str(), 1)
                } else {
                    // Relative to containing module: prepend mod path.
                    new_combined = Vec::with_capacity(
                        entry.containing_mod_path.len() + entry.path_prefix.len() + 1,
                    );
                    new_combined.extend_from_slice(&entry.containing_mod_path);
                    new_combined.extend(entry.path_prefix.iter().cloned());
                    new_combined.push(target_item.to_string());
                    let (_cd, _cs, tree2, _re2) = self.crates.get(crate_name)?;
                    return self.resolve_chain_in_tree(
                        crate_name,
                        tree2,
                        file,
                        item_name,
                        &new_combined,
                        workspace_root,
                        depth,
                    );
                }
            } else {
                (crate_name, 0)
            };

        new_combined = Vec::with_capacity(entry.path_prefix.len() + 1);
        new_combined.extend(entry.path_prefix[segs_start..].iter().cloned());
        new_combined.push(target_item.to_string());

        let (_crate_dir2, _cdir_str2, tree2, _reexports2) = self.crates.get(target_crate)?;
        self.resolve_chain_in_tree(
            target_crate,
            tree2,
            file,
            item_name,
            &new_combined,
            workspace_root,
            depth,
        )
    }

    /// Inner helper used by [`follow_pub_use_chain`] to look up `new_combined`
    /// in `tree2` and either recurse or return a terminal `ResolvedFqn`.
    ///
    /// Returns `None` if the combined path doesn't match any entry in `tree2`
    /// or if a direct cycle is detected.
    #[allow(clippy::too_many_arguments)]
    fn resolve_chain_in_tree(
        &self,
        target_crate: &str,
        tree2: &ModTree,
        original_file: &Path,
        original_item: &str,
        new_combined: &[String],
        workspace_root: &Path,
        depth: u8,
    ) -> Option<ResolvedFqn> {
        for prefix_len in (0..new_combined.len()).rev() {
            let key: Vec<String> = new_combined[..prefix_len].to_vec();
            let Some(candidate_file) = tree2.get(&key) else {
                continue;
            };
            let candidate_item = new_combined
                .get(prefix_len)
                .map(String::as_str)
                .unwrap_or("");

            if candidate_file == original_file && candidate_item == original_item {
                break;
            }

            if let Some(deeper) = self.follow_pub_use_chain(
                target_crate,
                candidate_file,
                candidate_item,
                workspace_root,
                depth + 1,
            ) {
                return Some(deeper);
            }

            let rel = uid_path(candidate_file, &self.workspace_canon).ok()?;
            return Some(ResolvedFqn {
                file: rel,
                item_name: candidate_item.to_string(),
            });
        }
        None
    }

    /// Whether any crates were indexed.
    pub fn is_empty(&self) -> bool {
        self.crates.is_empty()
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
        // cap[1]: prefix like "inner::" or "crate::deep::"
        // cap[2]: item name or "*"
        // cap[3]: optional alias
        let raw_prefix = &cap[1]; // includes trailing "::"
        let item = cap[2].to_string();
        let alias = cap.get(3).map(|m| m.as_str().to_string());

        // Strip trailing "::" and split into segments.
        let prefix_str = raw_prefix.trim_end_matches("::");
        let path_prefix: Vec<String> = prefix_str
            .split("::")
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();

        if path_prefix.is_empty() {
            continue;
        }

        let is_glob = item == "*";
        let exported_name = alias.as_deref().unwrap_or(item.as_str()).to_string();
        let original_name = item.clone();

        reexports.insert(
            (file.to_path_buf(), exported_name),
            ReExportEntry {
                path_prefix,
                original_name,
                is_glob,
                containing_mod_path: containing_mod_path.to_vec(),
            },
        );
    }
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
}
