//! registry.json schema and atomic IO. Spec §2.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryFile {
    pub version: u32,
    #[serde(default)]
    pub repos: Vec<RepoEntry>,
    #[serde(default)]
    pub groups: Vec<GroupEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RepoEntryRaw")]
pub struct RepoEntry {
    pub name: String,
    pub remote_url: String,
    pub worktree_path: String,
    pub index_dir_root: String,
    pub branches: Vec<BranchEntry>,
    /// Group memberships. Legacy `group: Option<String>` auto-migrates
    /// to a one-element Vec via `RepoEntryRaw`.
    pub groups: Vec<String>,
}

#[derive(Deserialize)]
struct RepoEntryRaw {
    name: String,
    remote_url: String,
    worktree_path: String,
    index_dir_root: String,
    #[serde(default)]
    branches: Vec<BranchEntry>,
    #[serde(default, alias = "group")]
    groups: Option<GroupsField>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum GroupsField {
    Vec(Vec<String>),
    Single(String),
}

impl From<RepoEntryRaw> for RepoEntry {
    fn from(raw: RepoEntryRaw) -> Self {
        let groups = match raw.groups {
            None => vec![],
            Some(GroupsField::Vec(v)) => v,
            Some(GroupsField::Single(s)) => vec![s],
        };
        RepoEntry {
            name: raw.name,
            remote_url: raw.remote_url,
            worktree_path: raw.worktree_path,
            index_dir_root: raw.index_dir_root,
            branches: raw.branches,
            groups,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchEntry {
    pub name: String,
    pub index_dir: String,
    pub indexed_at: String,
    pub node_count: u32,
    #[serde(default)]
    pub delta_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupEntry {
    pub name: String,
    pub members: Vec<String>,
}

impl RegistryFile {
    /// Locate the `(repo, branch)` pair whose `worktree_path` is the
    /// longest prefix of `cwd`. Used by hooks to translate the agent's
    /// current working directory into the on-disk index location
    /// (`<branch>.index_dir`) without re-running `gnx admin index`.
    ///
    /// When `branch_hint` is `Some`, prefer that branch within the
    /// matched repo entry; fall back to the most recently indexed
    /// branch if the hint doesn't match (so a freshly switched branch
    /// that hasn't been indexed yet still resolves to *some* index
    /// rather than failing the hook entirely).
    ///
    /// Returns `None` when no repo's `worktree_path` matches, when the
    /// repo has zero branches recorded, or when path comparison fails.
    pub fn find_by_cwd(
        &self,
        cwd: &std::path::Path,
        branch_hint: Option<&str>,
    ) -> Option<(&RepoEntry, &BranchEntry)> {
        // Canonicalize cwd once so symlinked work trees match the path
        // stored at index time (which came from `git rev-parse
        // --show-toplevel`, already canonicalized).
        let cwd_buf = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        let cwd_str = cwd_buf.to_string_lossy();
        let cwd_bytes = cwd_str.as_bytes();
        let sep = std::path::MAIN_SEPARATOR as u8;

        let repo = self
            .repos
            .iter()
            .filter(|r| {
                // Normalize a trailing separator on the registered path
                // before prefix-matching (`/work/alpha/` vs `/work/alpha`
                // both registered legitimately by different callers).
                let wp = r.worktree_path.trim_end_matches(std::path::MAIN_SEPARATOR);
                let wp_bytes = wp.as_bytes();
                if cwd_bytes == wp_bytes {
                    return true;
                }
                // cwd is strictly under wp: `wp_bytes` must be a prefix
                // *and* the next char must be the path separator. Pure
                // byte comparison — no per-call String allocation.
                cwd_bytes.len() > wp_bytes.len()
                    && cwd_bytes.starts_with(wp_bytes)
                    && cwd_bytes[wp_bytes.len()] == sep
            })
            .max_by_key(|r| r.worktree_path.len())?;
        // Most-recently-indexed fallback for both arms: the hint may
        // not match (freshly switched branch never indexed), and `None`
        // means caller had no git context at all.
        let most_recent = || {
            repo.branches
                .iter()
                .max_by(|a, b| a.indexed_at.cmp(&b.indexed_at))
        };
        let branch = match branch_hint {
            Some(h) => repo
                .branches
                .iter()
                .find(|b| b.name == h)
                .or_else(most_recent),
            None => most_recent(),
        }?;
        Some((repo, branch))
    }

    pub fn empty() -> Self {
        Self {
            version: 1,
            repos: vec![],
            groups: vec![],
        }
    }

    /// Atomic write: serialize → tmp → fsync → rename. If `path`
    /// already exists, copy current content to `<path>.bak` first.
    pub fn write_atomic(path: &Path, value: &RegistryFile) -> io::Result<()> {
        if path.exists() {
            let bak = bak_path(path);
            fs::copy(path, &bak)?;
        }
        crate::registry::io::atomic_write_json(path, value)
    }

    /// Read registry.json. On parse failure, try .bak. On both failures
    /// or missing file, return empty registry.
    pub fn read_or_empty(path: &Path) -> io::Result<Self> {
        match try_read(path) {
            Ok(v) => Ok(v),
            Err(_) => {
                let bak = bak_path(path);
                if bak.exists() {
                    match try_read(&bak) {
                        Ok(v) => Ok(v),
                        Err(_) => Ok(RegistryFile::empty()),
                    }
                } else {
                    Ok(RegistryFile::empty())
                }
            }
        }
    }
}

impl RegistryFile {
    /// Last-resort recovery: walk `~/.gnx/*/*/meta.json` and synthesize
    /// a fresh registry. Group memberships are LOST in this path
    /// (registry-only data) — operator must re-apply via `group_sync`.
    pub fn rebuild_from_disk(home_gnx: &Path) -> io::Result<Self> {
        use crate::registry::meta::BranchMeta;
        use std::collections::HashMap;
        let mut by_repo: HashMap<String, (String, String, String, Vec<BranchEntry>)> =
            HashMap::new();

        let repo_dirs = match fs::read_dir(home_gnx) {
            Ok(it) => it,
            Err(_) => return Ok(RegistryFile::empty()),
        };
        for repo_entry in repo_dirs.flatten() {
            if !repo_entry.path().is_dir() {
                continue;
            }
            let repo_name = match repo_entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            // Skip non-repo entries like _module, audit.log, etc.
            if repo_name.starts_with('_') || repo_name == "registry.json" {
                continue;
            }
            let branches_dir = repo_entry.path();
            let branch_iter = match fs::read_dir(&branches_dir) {
                Ok(it) => it,
                Err(_) => continue,
            };
            for branch_entry in branch_iter.flatten() {
                let meta_path = branch_entry.path().join("meta.json");
                if !meta_path.exists() {
                    continue;
                }
                let meta = match BranchMeta::read(&meta_path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let branch_name = match branch_entry.file_name().into_string() {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                let bre = BranchEntry {
                    name: branch_name,
                    index_dir: branch_entry.path().to_string_lossy().into(),
                    indexed_at: meta.indexed_at.clone(),
                    node_count: meta.node_count,
                    delta_size: meta.delta_size,
                };
                let entry = by_repo.entry(repo_name.clone()).or_insert_with(|| {
                    (
                        meta.worktree_path.clone(),
                        meta.remote_url.clone(),
                        meta.indexed_at.clone(),
                        vec![],
                    )
                });
                // If this branch is newer, take its worktree_path / remote_url
                if meta.indexed_at.as_str() > entry.2.as_str() {
                    entry.0 = meta.worktree_path.clone();
                    entry.1 = meta.remote_url.clone();
                    entry.2 = meta.indexed_at.clone();
                }
                entry.3.push(bre);
            }
        }

        let repos = by_repo
            .into_iter()
            .map(
                |(name, (worktree_path, remote_url, _latest, branches))| RepoEntry {
                    name: name.clone(),
                    remote_url,
                    worktree_path,
                    index_dir_root: home_gnx.join(&name).to_string_lossy().into(),
                    branches,
                    groups: vec![],
                },
            )
            .collect();

        Ok(RegistryFile {
            version: 1,
            repos,
            groups: vec![],
        })
    }
}

fn bak_path(path: &Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bak");
    std::path::PathBuf::from(s)
}

fn try_read(path: &Path) -> io::Result<RegistryFile> {
    if !path.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no registry"));
    }
    let bytes = fs::read(path)?;
    let parsed: RegistryFile = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if parsed.version != 1 {
        return Err(io::Error::other(format!(
            "unsupported registry version {} (expected 1); run `gnx analyze --force` to rebuild",
            parsed.version
        )));
    }
    Ok(parsed)
}

/// Remove user:pass from a remote URL. SSH URLs (`git@host:path`) and
/// already-clean URLs pass through unchanged.
pub fn strip_credentials(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        }
        Err(_) => url.to_string(),
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn migrate_single_group_to_vec() {
        let old_json = serde_json::json!({
            "version": 1,
            "repos": [{
                "name": "alpha",
                "remote_url": "https://example.com/alpha.git",
                "worktree_path": "/tmp/alpha",
                "index_dir_root": "/tmp/idx/alpha",
                "branches": [],
                "group": "backend"
            }],
            "groups": []
        });
        let parsed: RegistryFile = serde_json::from_value(old_json).unwrap();
        assert_eq!(parsed.repos[0].groups, vec!["backend".to_string()]);
    }

    #[test]
    fn migrate_null_group_to_empty_vec() {
        let old_json = serde_json::json!({
            "version": 1,
            "repos": [{
                "name": "alpha",
                "remote_url": "x",
                "worktree_path": "/tmp/a",
                "index_dir_root": "/tmp/i",
                "branches": [],
                "group": null
            }],
            "groups": []
        });
        let parsed: RegistryFile = serde_json::from_value(old_json).unwrap();
        assert!(parsed.repos[0].groups.is_empty());
    }

    #[test]
    fn new_format_round_trips() {
        let entry = RepoEntry {
            name: "x".into(),
            remote_url: "x".into(),
            worktree_path: "x".into(),
            index_dir_root: "x".into(),
            branches: vec![],
            groups: vec!["a".into(), "b".into()],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: RepoEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.groups, vec!["a", "b"]);
    }
}

#[cfg(test)]
mod find_by_cwd_tests {
    use super::*;
    use std::path::Path;

    fn br(name: &str, indexed_at: &str) -> BranchEntry {
        BranchEntry {
            name: name.into(),
            index_dir: format!("/home/.gnx/repo/{name}"),
            indexed_at: indexed_at.into(),
            node_count: 0,
            delta_size: 0,
        }
    }

    fn rg(reg: RegistryFile, repo: &str, wt: &str, branches: Vec<BranchEntry>) -> RegistryFile {
        let mut r = reg;
        r.repos.push(RepoEntry {
            name: repo.into(),
            remote_url: String::new(),
            worktree_path: wt.into(),
            index_dir_root: String::new(),
            branches,
            groups: vec![],
        });
        r
    }

    #[test]
    fn exact_worktree_match_resolves() {
        let reg = rg(
            RegistryFile::empty(),
            "alpha",
            "/work/alpha",
            vec![br("main", "2026-05-01")],
        );
        let (r, b) = reg.find_by_cwd(Path::new("/work/alpha"), None).unwrap();
        assert_eq!(r.name, "alpha");
        assert_eq!(b.name, "main");
    }

    #[test]
    fn subpath_under_worktree_resolves() {
        let reg = rg(
            RegistryFile::empty(),
            "alpha",
            "/work/alpha",
            vec![br("main", "2026-05-01")],
        );
        let (r, _) = reg
            .find_by_cwd(Path::new("/work/alpha/src/x.rs"), None)
            .unwrap();
        assert_eq!(r.name, "alpha");
    }

    #[test]
    fn longest_prefix_wins_when_nested_repos() {
        // /work/alpha registered, /work/alpha/sub also registered.
        // A file under /work/alpha/sub must map to the inner repo.
        let mut reg = rg(
            RegistryFile::empty(),
            "alpha",
            "/work/alpha",
            vec![br("main", "2026-05-01")],
        );
        reg = rg(
            reg,
            "alpha-sub",
            "/work/alpha/sub",
            vec![br("main", "2026-05-01")],
        );
        let (r, _) = reg
            .find_by_cwd(Path::new("/work/alpha/sub/x.rs"), None)
            .unwrap();
        assert_eq!(r.name, "alpha-sub");
    }

    #[test]
    fn branch_hint_picks_named_branch() {
        let reg = rg(
            RegistryFile::empty(),
            "alpha",
            "/work/alpha",
            vec![br("main", "2026-05-01"), br("feat/x", "2026-04-01")],
        );
        // Even though feat/x is older, branch_hint should select it.
        let (_, b) = reg
            .find_by_cwd(Path::new("/work/alpha"), Some("feat/x"))
            .unwrap();
        assert_eq!(b.name, "feat/x");
    }

    #[test]
    fn missing_branch_hint_falls_back_to_most_recent() {
        let reg = rg(
            RegistryFile::empty(),
            "alpha",
            "/work/alpha",
            vec![br("main", "2026-05-01"), br("feat/x", "2026-04-01")],
        );
        // Branch the cwd is currently on hasn't been indexed yet.
        let (_, b) = reg
            .find_by_cwd(Path::new("/work/alpha"), Some("never-indexed"))
            .unwrap();
        assert_eq!(b.name, "main", "newest indexed branch wins as fallback");
    }

    #[test]
    fn cwd_outside_any_worktree_yields_none() {
        let reg = rg(
            RegistryFile::empty(),
            "alpha",
            "/work/alpha",
            vec![br("main", "2026-05-01")],
        );
        assert!(reg.find_by_cwd(Path::new("/tmp/elsewhere"), None).is_none());
    }

    #[test]
    fn prefix_collision_at_non_separator_boundary_rejected() {
        // /work/alpha must NOT match cwd /work/alphabeta (no separator
        // between the suffix and the rest).
        let reg = rg(
            RegistryFile::empty(),
            "alpha",
            "/work/alpha",
            vec![br("main", "2026-05-01")],
        );
        assert!(reg
            .find_by_cwd(Path::new("/work/alphabeta"), None)
            .is_none());
    }

    #[test]
    fn no_branches_yields_none_even_when_path_matches() {
        let reg = rg(RegistryFile::empty(), "alpha", "/work/alpha", vec![]);
        assert!(reg.find_by_cwd(Path::new("/work/alpha"), None).is_none());
    }
}
