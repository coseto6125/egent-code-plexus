//! Zero-mtime shadow fragments (architecture plan, area 11 → lane F).
//!
//! An added `src/util.ts` shadows the tracked `src/util.js`, so the JS / TS
//! cascade in `reanalyze_files` re-parses `util.js` too. Only the dirty set
//! was statted before the parse, so `apply_l1_overlay_updates` records 0 as
//! the shadow's `mtime_ns`. No real file has mtime 0, so `accepted_fragments`
//! drops the fragment on every query and the cascade never reaches a view.
//!
//! The caller now stats the cascade set before the parse, so the manifest
//! holds the shadow's real mtime and the reader serves its fragment. Once
//! `util.ts` is gone, `util.js` is no longer a shadow, and each exit of
//! `ensure_fresh` (clean tree, freshness-gate skip, full refresh) retires its
//! entry: the reader would serve it while its mtime matches. The tests run
//! the real CLI path (`ecp find` on the dirty tree with one session id) and
//! read the session manifest.

mod common;

use common::{commit_all, ecp_bin, run_git, write};
use ecp_cli::session::overlay_reader::{accepted_fragments, decode_view_inputs};
use ecp_core::session::{layout, DirtyEvents};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

const SESSION_ID: &str = "zero-mtime-shadow";
const HOST_SESSION_VARS: [&str; 5] = [
    "ECP_SESSION_ID",
    "GEMINI_CLI_SESSION_ID",
    "CODEX_SESSION_ID",
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
];

fn ecp(repo: &Path, home: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new(ecp_bin());
    cmd.args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1");
    for key in HOST_SESSION_VARS {
        cmd.env_remove(key);
    }
    cmd.env("ECP_SESSION_ID", SESSION_ID);
    let out = cmd.output().expect("ecp failed to spawn");
    assert!(
        out.status.success(),
        "ecp {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The session dir holding a `l1/dirty_events.json`, found under `home`.
fn session_dir(home: &Path) -> PathBuf {
    let mut stack = vec![home.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "dirty_events.json")
                && dir.file_name().is_some_and(|n| n == "l1")
                && dir
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|n| n == SESSION_ID)
            {
                return dir.parent().unwrap().to_path_buf();
            }
        }
    }
    panic!("no session manifest under {}", home.display());
}

fn mtime_ns(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_nanos() as u64)
}

const UTIL_TS: &str = "export function helper(): number {\n  return 2;\n}\n";

fn setup(repo: &Path, home: &Path) {
    setup_with(repo, home, &[("src/util.ts", UTIL_TS)]);
}

/// Indexes the committed tree, writes `edits`, and runs one query.
fn setup_with(repo: &Path, home: &Path, edits: &[(&str, &str)]) {
    write(
        repo,
        "src/util.js",
        "function base() {\n  return 1;\n}\n\nexport function helper() {\n  return base();\n}\n",
    );
    write(
        repo,
        "src/main.js",
        "import { helper } from './util';\n\nexport function run() {\n  return helper();\n}\n",
    );
    run_git(repo, &["init", "-q", "-b", "main"]);
    commit_all(repo, "init");
    ecp(repo, home, &["admin", "index", "--repo", "."]);
    for (rel, content) in edits {
        write(repo, rel, content);
    }
    ecp(repo, home, &["find", "helper", "--format", "json"]);
}

fn served_paths(session: &Path, repo: &Path) -> BTreeSet<String> {
    let (accepted, _) = accepted_fragments(session, repo).unwrap();
    decode_view_inputs(&accepted)
        .into_iter()
        .map(|input| input.rel_path)
        .collect()
}

#[test]
fn test_accepted_fragments_shadow_cascade_mtime_reader_keeps_file() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup(repo.path(), home.path());

    let session = session_dir(home.path());
    let manifest = DirtyEvents::read(&layout::dirty_events_path(&session)).unwrap();
    assert!(
        manifest.entries.contains_key("src/util.ts"),
        "the dirty file itself must have a fragment: {:?}",
        manifest.entries.keys().collect::<Vec<_>>()
    );
    let served = served_paths(&session, repo.path());
    for rel in ["src/util.ts", "src/util.js"] {
        let entry = manifest
            .entries
            .get(rel)
            .unwrap_or_else(|| panic!("the shadow cascade must record {rel}"));
        assert_ne!(entry.mtime_ns, 0, "{rel} must not record mtime 0");
        assert_eq!(
            mtime_ns(&repo.path().join(rel)),
            Some(entry.mtime_ns),
            "{rel} must record the file's own mtime"
        );
        assert!(
            served.contains(rel),
            "the reader must serve {rel}: {served:?}"
        );
    }
}

const MAIN_EDITED: &str =
    "import { helper } from './util';\n\nexport function run() {\n  return helper() + 1;\n}\n";

/// Removes `src/util.ts`, so `src/util.js` is no longer a shadow of a dirty
/// path, after the setup query served both.
fn remove_shadowing_file(repo: &Path, session: &Path) {
    let served = served_paths(session, repo);
    assert!(
        served.contains("src/util.js"),
        "the setup must serve the shadow: {served:?}"
    );
    fs::remove_file(repo.join("src/util.ts")).unwrap();
}

#[test]
fn test_ensure_fresh_shadow_sibling_removed_clean_tree_retires_shadow() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup(repo.path(), home.path());
    let session = session_dir(home.path());
    remove_shadowing_file(repo.path(), &session);

    let out = ecp(
        repo.path(),
        home.path(),
        &["inspect", "--name", "helper", "--format", "json"],
    );

    assert_eq!(
        served_paths(&session, repo.path()),
        BTreeSet::new(),
        "a clean tree serves no fragment"
    );
    assert!(
        !out.contains("l1-overlay"),
        "util.js calls come from the base graph again: {out}"
    );
}

#[test]
fn test_ensure_fresh_shadow_sibling_removed_gate_skip_retires_shadow() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_with(
        repo.path(),
        home.path(),
        &[("src/util.ts", UTIL_TS), ("src/main.js", MAIN_EDITED)],
    );
    let session = session_dir(home.path());
    remove_shadowing_file(repo.path(), &session);

    // main.js is still dirty and its fragment current: the gate skips.
    ecp(
        repo.path(),
        home.path(),
        &["find", "helper", "--format", "json"],
    );

    assert_eq!(
        served_paths(&session, repo.path()),
        BTreeSet::from(["src/main.js".to_string()]),
    );
}

#[test]
fn test_ensure_fresh_shadow_sibling_removed_full_refresh_retires_shadow() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_with(
        repo.path(),
        home.path(),
        &[("src/util.ts", UTIL_TS), ("src/main.js", MAIN_EDITED)],
    );
    let session = session_dir(home.path());
    remove_shadowing_file(repo.path(), &session);
    std::thread::sleep(std::time::Duration::from_millis(50));
    write(
        repo.path(),
        "src/main.js",
        "import { helper } from './util';\n\nexport function run() {\n  return helper() + 2;\n}\n",
    );

    ecp(
        repo.path(),
        home.path(),
        &["find", "helper", "--format", "json"],
    );

    assert_eq!(
        served_paths(&session, repo.path()),
        BTreeSet::from(["src/main.js".to_string()]),
    );
}
