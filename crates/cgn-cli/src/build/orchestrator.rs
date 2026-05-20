//! L2 build orchestrator: src resolution → analyzer pipeline → atomic publish.
//!
//! Build lock at `<dirname>.building/.build.lock` (fs2 exclusive non-blocking).
//! Concurrent builders for the same SHA attach instead of duplicating work.

use crate::build::dirname_picker::pick_dirname;
use crate::commit_lookup::CommitIndex;
use crate::git::safe_exec;
use crate::repo_identity::repo_dir_name_for_cwd;
use cgn_core::registry::{
    resolve_home_cgn, CommitBuildMeta, EmbeddingStatus, RefRecord, RegistryFile, RepoAlias,
    RepoMeta, SourceType, BUILDER_FINGERPRINT,
};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static GENERATION_COUNTER: AtomicU64 = AtomicU64::new(0);

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

    let home_cgn = resolve_home_cgn();
    let repo_dir_name = repo_dir_name_for_cwd(worktree)?;
    let repo_root = home_cgn.join(&repo_dir_name);
    fs::create_dir_all(repo_root.join("commits"))?;

    let dirname = pick_dirname(worktree, &sha_hex)?;
    let commits_dir = repo_root.join("commits");
    let commit_dir = commits_dir.join(&dirname);
    let building = commits_dir.join(format!("{dirname}.building"));

    // Fast path: same SHA already built by a binary with a matching
    // fingerprint → reuse without touching the analyzer pipeline.
    // L2 is SHA-pure (v2 layout, PR #55); working-tree drift goes through
    // the L1 session overlay, not here.
    if let Some(attached) = attach_latest_if_fingerprint_matches(&commits_dir, &sha_hex) {
        return Ok(attached);
    }

    // Acquire build lock; attach pattern if locked
    fs::create_dir_all(&building)?;
    let lock_path = building.join(".build.lock");
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    if lock.try_lock_exclusive().is_err() {
        // Another builder owns this dir — wait for completion + return
        return wait_for_completion(&building, &commit_dir);
    }

    build_inside_locked(worktree, &sha_hex, &repo_root, &building, &commit_dir, lock)
}

/// Run the analyzer pipeline, write metadata, atomic-publish.
///
/// Pre-conditions (caller's responsibility):
/// - `building` dir exists
/// - exclusive build lock is held by the caller for the full call duration
/// - `repo_root.join("commits")` exists
///
/// Shared between `build_l2` (first build / SHA drift) and
/// `force_rebuild_l2` (after L1 invalidate + L2 drop). Both paths land in
/// the same atomic publish + repo_meta update.
///
/// `lock_guard` is the open `File` for `.build.lock` inside `building/`.
/// It is dropped (fd closed) immediately before the atomic rename so that
/// Windows does not reject the rename with os error 5 (Access Denied) due
/// to an open handle inside the directory being renamed.
pub(crate) fn build_inside_locked(
    worktree: &Path,
    sha_hex: &str,
    repo_root: &Path,
    building: &Path,
    commit_dir: &Path,
    lock_guard: File,
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

    // fsync + atomic publish. If an L2 for the same SHA already exists,
    // publish to a generation dir instead of touching the old reader-visible
    // directory. CommitIndex resolves same-SHA generations to the newest one.
    sync_all_files(building)?;
    // Windows refuses to rename a directory that contains any open file
    // handles (os error 5). Drop the lock fd now — the rename is the
    // publication event, so the lock is no longer needed after this point.
    drop(lock_guard);
    let publish_dir = publish_dir_for(commit_dir);
    rename_dir_with_retry(building, &publish_dir)?;
    let _ = cgn_core::registry::retire_dir_async(&publish_dir.join("_src"));

    update_repo_meta(repo_root, worktree, sha_hex)?;

    // Write the HEAD-SHA fingerprint next to the freshly published graph.bin
    // so subsequent read commands can short-circuit `auto_ensure::ensure_index`
    // without walking the working tree. Last step on the build path, detached
    // to a background thread so the build's wall-clock isn't bumped by the
    // tiny 41-byte write.
    crate::auto_ensure::write_head_sha_sidecar_with_sha(&publish_dir.join("graph.bin"), sha_hex);

    Ok(BuildResult {
        commit_dir: publish_dir,
        sha_hex: sha_hex.to_string(),
        source_type,
    })
}

pub(crate) fn attach_latest_if_fingerprint_matches(
    commits_dir: &Path,
    sha_hex: &str,
) -> Option<BuildResult> {
    let sha = sha_bytes(sha_hex)?;
    let idx = CommitIndex::scan(commits_dir).ok()?;
    let dir = idx.find(&sha)?;
    attach_if_fingerprint_matches(&commits_dir.join(dir))
}

/// Cheap pre-build check: if `commit_dir/meta.json` exists and its
/// `builder_fingerprint` matches the current binary, the published L2 at
/// this SHA was made by an equivalent build — return it instead of
/// rebuilding. Shared between `build_l2` (skip-if-exists fast path) and
/// `force_rebuild_l2` (after `wait_for_completion`, lets N concurrent
/// `--force` callers attach to one winner instead of each rebuilding).
pub(crate) fn attach_if_fingerprint_matches(commit_dir: &Path) -> Option<BuildResult> {
    if !commit_dir.join("meta.json").is_file() {
        return None;
    }
    let meta = CommitBuildMeta::read(&commit_dir.join("meta.json")).ok()?;
    if meta.builder_fingerprint.as_deref() != Some(BUILDER_FINGERPRINT) {
        return None;
    }
    // Back-fill the HEAD-SHA sidecar for graphs published by binaries that
    // pre-date the auto_ensure shortcut. One-shot until the next rebuild.
    crate::auto_ensure::write_head_sha_sidecar_with_sha(&commit_dir.join("graph.bin"), &meta.sha);
    Some(BuildResult {
        commit_dir: commit_dir.to_path_buf(),
        sha_hex: meta.sha,
        source_type: meta.source_type,
    })
}

fn publish_dir_for(base_commit_dir: &Path) -> PathBuf {
    if !base_commit_dir.exists() {
        return base_commit_dir.to_path_buf();
    }
    let name = base_commit_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("commit");
    let generation = format!(
        "{name}.gen.{}.{}.{}",
        chrono::Utc::now().timestamp_millis(),
        std::process::id(),
        GENERATION_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    base_commit_dir.with_file_name(generation)
}

fn sha_bytes(sha_hex: &str) -> Option<[u8; 20]> {
    let mut sha = [0u8; 20];
    hex::decode_to_slice(sha_hex, &mut sha).ok()?;
    Some(sha)
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
        .args(["-x", "-f", "-", "-C", dest.to_string_lossy().as_ref()])
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
            if entry.file_name() == ".build.lock" {
                continue;
            }
            let f = File::open(entry.path())?;
            f.sync_all()?;
        }
    }
    Ok(())
}

fn rename_dir_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::time::Duration;

        let mut last_err = None;
        for attempt in 0..50 {
            match fs::rename(from, to) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    let raw = err.raw_os_error();
                    if raw != Some(5) && raw != Some(32) {
                        return Err(err);
                    }
                    last_err = Some(err);
                    std::thread::sleep(Duration::from_millis(1 + attempt / 10));
                }
            }
        }
        Err(last_err.unwrap_or_else(io::Error::last_os_error))
    }
    #[cfg(not(windows))]
    fs::rename(from, to)
}

pub(crate) fn update_repo_meta(repo_root: &Path, worktree: &Path, sha: &str) -> io::Result<()> {
    let meta_path = repo_root.join("meta.json");
    let lock_path = repo_root.join(".meta.lock");
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
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

    // Sync the global registry. Without this, `contracts --repo @all`,
    // `coverage`, and any other Registry-backed reader is blind to repos
    // indexed since the registry was last (manually) rebuilt — `rebuild_from_disk`
    // was the only writer the build path ever touched, and it has zero callers
    // outside the v2 migration probe. Lock order: caller holds per-repo
    // `.meta.lock` (above) → we acquire registry lock here. Single-direction
    // nesting: every other registry writer takes only the registry lock.
    let repo_dir_name = repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::other("repo_root has no final component"))?;
    let home_cgn = repo_root
        .parent()
        .ok_or_else(|| io::Error::other("repo_root has no parent (home_cgn)"))?;
    RegistryFile::upsert_repo_atomic(home_cgn, RepoAlias::from_repo_meta(repo_dir_name, &rm))?;
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
    let parent = commit_dir
        .parent()
        .ok_or_else(|| io::Error::other("commit dir has no parent"))?;
    let parsed = commit_dir
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| cgn_core::registry::CommitDirName::parse(name).ok())
        .ok_or_else(|| io::Error::other("attached builder target dirname is invalid"))?;
    let idx = CommitIndex::scan(parent)?;
    let Some(name) = idx.find(&parsed.sha) else {
        return Err(io::Error::other("attached builder failed to publish"));
    };
    let commit_dir = parent.join(name);
    let meta_path = commit_dir.join("meta.json");
    let meta = CommitBuildMeta::read(&meta_path)?;
    Ok(BuildResult {
        commit_dir: commit_dir.to_path_buf(),
        sha_hex: meta.sha,
        source_type: meta.source_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cgn_core::registry::{CommitBuildMeta, EmbeddingStatus, RefRecord, SourceType};
    use tempfile::TempDir;

    #[test]
    fn wait_for_completion_prefers_latest_generation_for_same_sha() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path().join("commits");
        fs::create_dir_all(&parent).unwrap();

        let sha_hex = "0123456789abcdef0123456789abcdef01234567";
        let base = parent.join(format!("branch_main__{sha_hex}"));
        let gen_dir = parent.join(format!("branch_main__{sha_hex}.gen.1.2.3"));
        fs::create_dir_all(&base).unwrap();
        fs::create_dir_all(&gen_dir).unwrap();

        let meta = CommitBuildMeta {
            version: 1,
            sha: sha_hex.to_string(),
            source_type: SourceType::Branch,
            source_id: Some("main".to_string()),
            built_from_worktree: "repo".to_string(),
            built_at: "2026-05-20T00:00:00Z".to_string(),
            parent_sha: None,
            node_count: 1,
            embedding_status: EmbeddingStatus::None,
            refs_at_build: vec![RefRecord {
                ref_name: "refs/heads/main".to_string(),
                seen_at: "2026-05-20T00:00:00Z".to_string(),
            }],
            refs_seen_since: vec![],
            builder_fingerprint: Some(BUILDER_FINGERPRINT.to_string()),
        };
        CommitBuildMeta::write_atomic(&base.join("meta.json"), &meta).unwrap();
        CommitBuildMeta::write_atomic(&gen_dir.join("meta.json"), &meta).unwrap();

        let building = parent.join(format!("branch_main__{sha_hex}.building"));
        let result = wait_for_completion(&building, &base).unwrap();

        assert_eq!(result.commit_dir, gen_dir);
        assert_eq!(result.sha_hex, sha_hex);
    }
}
