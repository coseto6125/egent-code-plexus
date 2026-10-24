//! Zero-mtime shadow fragments (architecture plan, area 11 → lane F).
//!
//! An added `src/util.ts` shadows the tracked `src/util.js`, so the JS / TS
//! cascade in `reanalyze_files` re-parses `util.js` too. Only the dirty set
//! was statted before the parse, so `apply_l1_overlay_updates` records 0 as
//! the shadow's `mtime_ns`. No real file has mtime 0, so `accepted_fragments`
//! drops the fragment on every query and the cascade never reaches a view.
//!
//! The test runs the real CLI path (`ecp find` on the dirty tree with one
//! session id) and reduces the session manifest to lines:
//!
//! - `zero-mtime <path>`: the manifest records mtime 0 for the file.
//! - `stale-mtime <path>`: it records a non-zero mtime other than the file's.
//! - `reader-drops <path>`: the reader (`accepted_fragments` and
//!   `decode_view_inputs`) does not serve its fragment to the view.
//!
//! The lines must equal `tests/baselines/shadow_zero_mtime.txt` exactly; the
//! lane that repairs the mtime capture deletes that baseline.

#[path = "common/baseline.rs"]
mod baseline;
mod common;

use common::{commit_all, ecp_bin, run_git, write};
use ecp_cli::session::overlay_reader::{accepted_fragments, decode_view_inputs};
use ecp_core::session::DirtyFiles;
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

fn ecp(repo: &Path, home: &Path, args: &[&str]) {
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
}

/// The session dir holding a `dirty_files.json`, found under `home`.
fn session_dir(home: &Path) -> PathBuf {
    let mut stack = vec![home.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "dirty_files.json")
                && dir.file_name().is_some_and(|n| n == SESSION_ID)
            {
                return dir;
            }
        }
    }
    panic!("no session manifest under {}", home.display());
}

fn mtime_ns(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_nanos() as u64)
}

#[test]
fn test_accepted_fragments_shadow_cascade_mtime_matches_baseline() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write(
        repo.path(),
        "src/util.js",
        "export function helper() {\n  return 1;\n}\n",
    );
    write(
        repo.path(),
        "src/main.js",
        "import { helper } from './util';\n\nexport function run() {\n  return helper();\n}\n",
    );
    run_git(repo.path(), &["init", "-q", "-b", "main"]);
    commit_all(repo.path(), "init");
    ecp(repo.path(), home.path(), &["admin", "index", "--repo", "."]);

    write(
        repo.path(),
        "src/util.ts",
        "export function helper(): number {\n  return 2;\n}\n",
    );
    ecp(
        repo.path(),
        home.path(),
        &["find", "helper", "--format", "json"],
    );

    let session = session_dir(home.path());
    let manifest = DirtyFiles::read(&session.join("dirty_files.json")).unwrap();
    assert!(
        manifest.entries.contains_key("src/util.ts"),
        "the dirty file itself must have a fragment: {:?}",
        manifest.entries.keys().collect::<Vec<_>>()
    );
    assert!(
        manifest.entries.contains_key("src/util.js"),
        "the shadow cascade must re-parse util.js: {:?}",
        manifest.entries.keys().collect::<Vec<_>>()
    );

    let (accepted, _) = accepted_fragments(&session, repo.path()).unwrap();
    let served: BTreeSet<String> = decode_view_inputs(&accepted)
        .into_iter()
        .map(|input| input.rel_path)
        .collect();
    let mut observed = BTreeSet::new();
    for (rel, entry) in &manifest.entries {
        if entry.mtime_ns == 0 {
            observed.insert(format!("zero-mtime {rel}"));
        } else if mtime_ns(&repo.path().join(rel)) != Some(entry.mtime_ns) {
            observed.insert(format!("stale-mtime {rel}"));
        }
        if !served.contains(rel) {
            observed.insert(format!("reader-drops {rel}"));
        }
    }

    baseline::check(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/baselines/shadow_zero_mtime.txt"),
        "lane F (area 5 shadow-cascade mtime)",
        &observed,
    );
}
