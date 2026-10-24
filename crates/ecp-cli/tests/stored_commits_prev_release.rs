//! A `commits/` layout the previous release wrote, read and swept by today's
//! code.
//!
//! `fixtures/stored-commits-by-0.16.4/` holds three dirs of one SHA that
//! ecp 0.16.4 wrote: a bare published dir, a `.gen.` generation and a legacy
//! `<base>.building/` staging dir. Read its README first: it gives the pass
//! criterion, what was arranged by hand, and what the sample cannot show.
//!
//! The index test has two arms, picked by comparing today's builder
//! fingerprint with the writer's. Same fingerprint: today's binary must use
//! the stored generation and build nothing. Different fingerprint: it must
//! build one new generation and make it current. A slice that bumps the
//! fingerprint moves the test to the second arm without an edit.

use ecp_cli::admin::gc::sweep_stale_generations;
use ecp_cli::commit_lookup::{CommitDirListing, CommitIndex, StagingShape};
use ecp_cli::repo_identity::repo_dir_name_for_cwd;
use ecp_core::registry::BUILDER_FINGERPRINT;
use filetime::FileTime;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

const WRITER_FINGERPRINT: &str = "v0.16.4+schema20";
const SHA: &str = "220864493c29195a6e8b6c5cca462e7566a6ef99";
const BARE: &str = "branch_main__220864493c29195a6e8b6c5cca462e7566a6ef99";
const GEN: &str =
    "branch_main__220864493c29195a6e8b6c5cca462e7566a6ef99.gen.1792872205014.1314204.0";
const LEGACY: &str = "branch_main__220864493c29195a6e8b6c5cca462e7566a6ef99.building";
const HOUR: Duration = Duration::from_secs(3600);
const EIGHT_DAYS: Duration = Duration::from_secs(8 * 86_400);

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stored-commits-by-0.16.4")
}

fn source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stored-by-0.16.4/source/committed")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Every file below `dir`, by relative path, with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Sets the mtime of `path` and, for a dir, of everything below it.
fn backdate(path: &Path, then: SystemTime) {
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap().flatten() {
            backdate(&entry.path(), then);
        }
    }
    filetime::set_file_mtime(path, FileTime::from_system_time(then)).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(args)
        .env("GIT_AUTHOR_DATE", "2026-10-25T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-10-25T00:00:00Z")
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git available");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

struct Layout {
    _tmp: TempDir,
    wt: PathBuf,
    ecp_home: PathBuf,
    user_home: PathBuf,
    repo_root: PathBuf,
    commits: PathBuf,
}

/// The writer's repo at the writer's SHA, and the fixture's `commits/` under
/// a private `ECP_HOME`. Published dirs are one hour old, past gc's in-flight
/// guard; the legacy dir is `legacy_age` old.
fn layout(legacy_age: Duration) -> Layout {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    copy_tree(&source(), &wt);
    git(&wt, &["init", "-q", "-b", "main"]);
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "init"]);
    assert_eq!(
        git(&wt, &["rev-parse", "HEAD"]),
        SHA,
        "fixture repo drifted"
    );

    let ecp_home = tmp.path().join("ecp-home");
    let user_home = tmp.path().join("user-home");
    fs::create_dir_all(&user_home).unwrap();
    let repo_root = ecp_home.join(repo_dir_name_for_cwd(&wt).unwrap());
    let commits = repo_root.join("commits");
    copy_tree(&fixture().join("commits"), &commits);

    // The writer recorded its own scratch repo, which no longer exists; a slot
    // whose recorded git root is gone is rejected by design.
    let root = dunce::canonicalize(&wt).unwrap();
    for dir in [BARE, GEN] {
        let path = commits.join(dir).join("meta.json");
        let mut meta: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        meta["built_from_worktree"] = serde_json::Value::from(root.to_string_lossy().as_ref());
        fs::write(&path, serde_json::to_vec_pretty(&meta).unwrap()).unwrap();
    }

    let now = SystemTime::now();
    for dir in [BARE, GEN] {
        backdate(&commits.join(dir), now - HOUR);
    }
    backdate(&commits.join(LEGACY), now - legacy_age);
    Layout {
        _tmp: tmp,
        wt,
        ecp_home,
        user_home,
        repo_root,
        commits,
    }
}

fn sha_bytes() -> [u8; 20] {
    let mut out = [0u8; 20];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&SHA[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

/// The entries of `commits/` other than `.locks`, which today's binary adds.
fn entries(commits: &Path) -> BTreeSet<String> {
    fs::read_dir(commits)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name != ".locks")
        .collect()
}

fn names(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn legacy_unchanged(l: &Layout) {
    assert_eq!(
        snapshot(&l.commits.join(LEGACY)),
        snapshot(&fixture().join("commits").join(LEGACY)),
        "the legacy staging dir must keep the bytes 0.16.4 wrote"
    );
}

fn ecp(l: &Layout, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ecp"));
    cmd.args(args)
        .current_dir(&l.wt)
        .env("ECP_HOME", &l.ecp_home)
        .env("HOME", &l.user_home)
        .env("ECP_SKIP_BG_REBUILD", "1")
        .env("ECP_NO_PROGRESS", "1");
    // A host session id from the test runner would send the query through
    // that session's overlay.
    for key in [
        "ECP_SESSION_ID",
        "GEMINI_CLI_SESSION_ID",
        "CODEX_SESSION_ID",
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
    ] {
        cmd.env_remove(key);
    }
    let out = cmd.output().expect("run ecp");
    assert!(
        out.status.success(),
        "ecp {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Readers pick the `.gen.` dir, the dir 0.16.4 picks (README: its query
/// wrote `graph.bin.worktree` there and nowhere else).
#[test]
fn test_commit_dir_listing_layout_by_0_16_4_current_is_gen_dir() {
    let l = layout(HOUR);
    let sha = sha_bytes();

    let listing = CommitDirListing::read(&l.commits).unwrap();

    assert!(listing.complete);
    assert_eq!(listing.current(&sha).map(|d| d.name.as_str()), Some(GEN));
    let staging: Vec<(&str, StagingShape)> = listing
        .staging
        .iter()
        .map(|s| (s.name.as_str(), s.shape))
        .collect();
    assert_eq!(staging, vec![(LEGACY, StagingShape::Legacy)]);
    assert_eq!(CommitIndex::scan(&l.commits).unwrap().find(&sha), Some(GEN));
}

/// Same fingerprint: `admin index` and a query use the stored generation and
/// build nothing. Different fingerprint: one new generation is built and is
/// current. Either way the legacy dir keeps its bytes.
#[test]
fn test_admin_index_layout_by_0_16_4_attaches_or_rebuilds_by_fingerprint() {
    let l = layout(HOUR);

    let out = ecp(&l, &["admin", "index", "--repo", "."]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    if BUILDER_FINGERPRINT == WRITER_FINGERPRINT {
        assert!(
            stderr.contains("l2.exists") && !stderr.contains("l2.built"),
            "a graph 0.16.4 built at HEAD must be reused: {stderr}"
        );
        assert_eq!(entries(&l.commits), names(&[BARE, GEN, LEGACY]));

        let found = ecp(&l, &["find", "compute", "--format", "json"]);

        let stderr = String::from_utf8_lossy(&found.stderr);
        assert!(
            !stderr.contains("l2.built") && !stderr.contains("l2.rebuilt"),
            "the query rebuilt a graph 0.16.4 built at HEAD: {stderr}"
        );
        let v: serde_json::Value = serde_json::from_slice(&found.stdout).unwrap();
        assert_eq!(v["found"], true, "{v}");
        let files: Vec<&str> = v["matches"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["file"].as_str())
            .collect();
        assert!(files.contains(&"pkg/core.py"), "{v}");
        assert_eq!(entries(&l.commits), names(&[BARE, GEN, LEGACY]));
    } else {
        assert!(
            stderr.contains("l2.built"),
            "a graph of another fingerprint must be rebuilt: {stderr}"
        );
        let listing = CommitDirListing::read(&l.commits).unwrap();
        let built: Vec<&str> = listing
            .dirs
            .iter()
            .map(|d| d.name.as_str())
            .filter(|name| *name != BARE && *name != GEN)
            .collect();
        assert_eq!(built.len(), 1, "{built:?}");
        assert_eq!(
            listing.current(&sha_bytes()).map(|d| d.name.as_str()),
            Some(built[0])
        );
    }
    legacy_unchanged(&l);
}

/// A legacy staging dir written in the last 7 days may still belong to a
/// running 0.16.4 build of that SHA, so gc retires nothing of the SHA.
#[test]
fn test_sweep_stale_generations_layout_by_0_16_4_recent_legacy_retires_nothing() {
    let l = layout(HOUR);

    sweep_stale_generations(&l.repo_root, false).unwrap();

    assert_eq!(entries(&l.commits), names(&[BARE, GEN, LEGACY]));
    legacy_unchanged(&l);
}

/// Once the legacy dir is idle, gc retires the older generation (the bare
/// dir) and keeps the current `.gen.` dir and the legacy dir.
#[test]
fn test_sweep_stale_generations_layout_by_0_16_4_idle_legacy_retires_bare_only() {
    let l = layout(EIGHT_DAYS);

    sweep_stale_generations(&l.repo_root, false).unwrap();

    assert_eq!(entries(&l.commits), names(&[GEN, LEGACY]));
    assert_eq!(
        fs::read(l.commits.join(GEN).join("graph.bin")).unwrap(),
        fs::read(fixture().join("commits").join(GEN).join("graph.bin")).unwrap()
    );
    legacy_unchanged(&l);
}
