//! L2 build orchestrator: src resolution → analyzer pipeline → atomic publish.
//!
//! Build lock at `<dirname>.building/.build.lock` (fs2 exclusive non-blocking).
//! Concurrent builders for the same SHA attach instead of duplicating work.

use crate::build::dirname_picker::pick_dirname;
use crate::git::safe_exec;
use crate::repo_identity::repo_dir_name_for_cwd;
use fs2::FileExt;
use graph_nexus_core::registry::{
    resolve_home_gnx, CommitBuildMeta, EmbeddingStatus, RefRecord, RepoMeta, SourceType,
    BUILDER_FINGERPRINT,
};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

pub struct BuildResult {
    // Read only by `tests/build_orchestrator.rs`; bin callers ignore it today.
    #[allow(dead_code)]
    pub commit_dir: PathBuf,
    pub sha_hex: String,
    pub source_type: SourceType,
}

pub fn build_l2(worktree: &Path, target_sha: Option<&str>) -> io::Result<BuildResult> {
    let sha_hex = match target_sha {
        Some(s) => s.to_string(),
        None => head_sha_hex(worktree)?,
    };
    if sha_hex.len() != 40 || !sha_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(io::Error::other(format!("invalid sha: {sha_hex}")));
    }

    let home_gnx = resolve_home_gnx();
    let repo_dir_name = repo_dir_name_for_cwd(worktree)?;
    let repo_root = home_gnx.join(&repo_dir_name);
    fs::create_dir_all(repo_root.join("commits"))?;

    let dirname = pick_dirname(worktree, &sha_hex)?;
    let commit_dir = repo_root.join("commits").join(&dirname);
    let building = repo_root
        .join("commits")
        .join(format!("{dirname}.building"));

    // Fast path: same SHA already built by a binary with a matching
    // fingerprint → reuse without touching the analyzer pipeline.
    // L2 is SHA-pure (v2 layout, PR #55); working-tree drift goes through
    // the L1 session overlay, not here.
    if commit_dir.join("meta.json").is_file() {
        if let Ok(meta) = CommitBuildMeta::read(&commit_dir.join("meta.json")) {
            if meta.builder_fingerprint.as_deref() == Some(BUILDER_FINGERPRINT) {
                return Ok(BuildResult {
                    commit_dir,
                    sha_hex,
                    source_type: meta.source_type,
                });
            }
        }
    }

    // Acquire build lock; attach pattern if locked
    fs::create_dir_all(&building)?;
    let lock_path = building.join(".build.lock");
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)?;
    if lock.try_lock_exclusive().is_err() {
        // Another builder owns this dir — wait for completion + return
        return wait_for_completion(&building, &commit_dir);
    }

    build_inside_locked(worktree, &sha_hex, &repo_root, &building, &commit_dir)
}

/// Run the analyzer pipeline, write metadata, atomic-publish.
///
/// Pre-conditions (caller's responsibility):
/// - `building` dir exists, `commit_dir` does not yet exist
/// - exclusive build lock is held by the caller for the full call duration
/// - `repo_root.join("commits")` exists
///
/// Shared between `build_l2` (first build / SHA drift) and
/// `force_rebuild_l2` (after L1 invalidate + L2 drop). Both paths land in
/// the same atomic publish + repo_meta update.
pub(crate) fn build_inside_locked(
    worktree: &Path,
    sha_hex: &str,
    repo_root: &Path,
    building: &Path,
    commit_dir: &Path,
) -> io::Result<BuildResult> {
    let src_root = if worktree_clean_and_head_matches(worktree, sha_hex)? {
        worktree.to_path_buf()
    } else {
        let src = building.join("_src");
        fs::create_dir_all(&src)?;
        git_archive_to(worktree, sha_hex, &src)?;
        src
    };

    // Analyzer pipeline. `repo_root` doubles as the persistent parse_cache
    // root — cache entries live in `<repo_root>/parse_cache/<fp>/` and
    // survive across L2 commit_dirs as long as the file content (and binary
    // build) is unchanged.
    let node_count = crate::commands::admin::index::run_analyzer_for_paths(
        &src_root,
        building,
        Some(repo_root),
    )?;

    let refs_at_build = collect_refs(worktree, sha_hex)?;
    let source_type = source_type_from_refs(&refs_at_build);
    let source_id = source_id_from_refs(&refs_at_build);
    let parent = parent_sha(worktree, sha_hex).ok();

    let meta = CommitBuildMeta {
        version: 1,
        sha: sha_hex.to_string(),
        source_type,
        source_id,
        built_from_worktree: worktree.to_string_lossy().into(),
        built_at: chrono::Utc::now().to_rfc3339(),
        parent_sha: parent,
        node_count: node_count as u32,
        embedding_status: EmbeddingStatus::None,
        refs_at_build,
        refs_seen_since: vec![],
        builder_fingerprint: Some(BUILDER_FINGERPRINT.to_string()),
    };
    CommitBuildMeta::write_atomic(&building.join("meta.json"), &meta)?;

    // fsync + atomic publish. A stale commit_dir from an earlier binary
    // (fingerprint mismatch) is swept aside before the rename — Linux
    // refuses to rename onto a non-empty directory. force_rebuild_l2 also
    // does its own drop earlier, so this branch covers both paths.
    sync_all_files(building)?;
    if commit_dir.exists() {
        fs::remove_dir_all(commit_dir)?;
    }
    fs::rename(building, commit_dir)?;
    let _ = fs::remove_dir_all(commit_dir.join("_src"));

    update_repo_meta(repo_root, worktree, sha_hex)?;

    Ok(BuildResult {
        commit_dir: commit_dir.to_path_buf(),
        sha_hex: sha_hex.to_string(),
        source_type,
    })
}

pub(crate) fn head_sha_hex(worktree: &Path) -> io::Result<String> {
    let out = safe_exec::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("git rev-parse HEAD failed"));
    }
    Ok(std::str::from_utf8(&out.stdout)
        .map_err(io::Error::other)?
        .trim()
        .to_string())
}

pub(crate) fn worktree_clean_and_head_matches(worktree: &Path, sha: &str) -> io::Result<bool> {
    if head_sha_hex(worktree)? != sha {
        return Ok(false);
    }
    let out = safe_exec::git()
        .args(["diff-index", "--quiet", "HEAD"])
        .current_dir(worktree)
        .output()?;
    Ok(out.status.success())
}

pub(crate) fn git_archive_to(worktree: &Path, sha: &str, dest: &Path) -> io::Result<()> {
    let archive = safe_exec::git()
        .args(["archive", "--format=tar", sha])
        .current_dir(worktree)
        .output()?;
    if !archive.status.success() {
        return Err(io::Error::other("git archive failed"));
    }
    let mut child = std::process::Command::new("tar")
        .args(["-x", "-C", dest.to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    use std::io::Write;
    child.stdin.as_mut().unwrap().write_all(&archive.stdout)?;
    let s = child.wait()?;
    if !s.success() {
        return Err(io::Error::other("tar extract failed"));
    }
    Ok(())
}

pub(crate) fn collect_refs(worktree: &Path, sha: &str) -> io::Result<Vec<RefRecord>> {
    let out = safe_exec::git()
        .args(["for-each-ref", "--points-at", sha, "--format=%(refname)"])
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        return Ok(vec![]);
    }
    let now = chrono::Utc::now().to_rfc3339();
    Ok(std::str::from_utf8(&out.stdout)
        .unwrap_or("")
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| RefRecord {
            ref_name: l.to_string(),
            seen_at: now.clone(),
        })
        .collect())
}

pub(crate) fn source_type_from_refs(refs: &[RefRecord]) -> SourceType {
    if refs.iter().any(|r| r.ref_name.starts_with("refs/heads/")) {
        return SourceType::Branch;
    }
    if refs.iter().any(|r| r.ref_name.starts_with("refs/tags/")) {
        return SourceType::Tag;
    }
    if refs.iter().any(|r| {
        r.ref_name.starts_with("refs/pull/") || r.ref_name.starts_with("refs/merge-requests/")
    }) {
        return SourceType::Pr;
    }
    SourceType::Commit
}

pub(crate) fn source_id_from_refs(refs: &[RefRecord]) -> Option<String> {
    for r in refs {
        if let Some(b) = r.ref_name.strip_prefix("refs/heads/") {
            return Some(b.to_string());
        }
    }
    for r in refs {
        if let Some(t) = r.ref_name.strip_prefix("refs/tags/") {
            return Some(t.to_string());
        }
    }
    for r in refs {
        if let Some(rest) = r
            .ref_name
            .strip_prefix("refs/pull/")
            .or_else(|| r.ref_name.strip_prefix("refs/merge-requests/"))
        {
            if let Some(n) = rest.split('/').next() {
                return Some(n.to_string());
            }
        }
    }
    None
}

pub(crate) fn parent_sha(worktree: &Path, sha: &str) -> io::Result<String> {
    let out = safe_exec::git()
        .args(["rev-parse", &format!("{sha}^")])
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("no parent"));
    }
    Ok(std::str::from_utf8(&out.stdout)
        .map_err(io::Error::other)?
        .trim()
        .to_string())
}

pub(crate) fn sync_all_files(dir: &Path) -> io::Result<()> {
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.file_type().is_file() {
            let f = File::open(entry.path())?;
            f.sync_all()?;
        }
    }
    Ok(())
}

pub(crate) fn update_repo_meta(repo_root: &Path, worktree: &Path, sha: &str) -> io::Result<()> {
    let meta_path = repo_root.join("meta.json");
    let lock_path = repo_root.join(".meta.lock");
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)?;
    lock.lock_exclusive()?;

    let mut rm = if meta_path.exists() {
        RepoMeta::read(&meta_path)?
    } else {
        RepoMeta {
            version: 1,
            common_dir: git_common_dir_string(worktree)?,
            remote_url: git_remote_url(worktree).ok(),
            aliases: vec![],
            known_refs: Default::default(),
            last_built_sha: None,
            total_size_bytes: 0,
            last_touched: chrono::Utc::now().to_rfc3339(),
        }
    };
    rm.last_built_sha = Some(sha.to_string());
    rm.last_touched = chrono::Utc::now().to_rfc3339();
    rm.total_size_bytes = dir_size(repo_root)?;
    RepoMeta::write_atomic(&meta_path, &rm)?;
    Ok(())
}

fn git_common_dir_string(worktree: &Path) -> io::Result<String> {
    let out = safe_exec::git()
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(worktree)
        .output()?;
    let s = std::str::from_utf8(&out.stdout)
        .map_err(io::Error::other)?
        .trim();
    let p = std::path::PathBuf::from(s);
    let resolved = if p.is_absolute() { p } else { worktree.join(p) };
    Ok(std::fs::canonicalize(resolved)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| s.to_string()))
}

fn git_remote_url(worktree: &Path) -> io::Result<String> {
    let out = safe_exec::git()
        .args(["remote", "get-url", "origin"])
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("no origin remote"));
    }
    Ok(std::str::from_utf8(&out.stdout)
        .map_err(io::Error::other)?
        .trim()
        .to_string())
}

fn dir_size(dir: &Path) -> io::Result<u64> {
    let mut total = 0;
    for e in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
    {
        if e.file_type().is_file() {
            total += e.metadata()?.len();
        }
    }
    Ok(total)
}

pub(crate) fn wait_for_completion(building: &Path, commit_dir: &Path) -> io::Result<BuildResult> {
    let start = std::time::Instant::now();
    while building.exists() {
        if start.elapsed() > std::time::Duration::from_secs(600) {
            return Err(io::Error::other("build attach timeout"));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    if !commit_dir.exists() {
        return Err(io::Error::other("attached builder failed to publish"));
    }
    let meta_path = commit_dir.join("meta.json");
    let meta = CommitBuildMeta::read(&meta_path)?;
    Ok(BuildResult {
        commit_dir: commit_dir.to_path_buf(),
        sha_hex: meta.sha,
        source_type: meta.source_type,
    })
}
