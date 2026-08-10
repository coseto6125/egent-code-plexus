//! SOFT concerns end-to-end: a peer editing a graph neighbour of our dirty
//! symbols must reach our inbox.
//!
//! The watcher classified SOFT against an always-empty impact cache until the
//! graph was wired in, so only HARD overlaps ever fired. Here bob is dirty on
//! `caller_one` in one file and alice touches `target_fn` in another, which bob
//! reaches only through a Calls edge. Separate files matter: HARD is a shared
//! dirty file and wins over SOFT, so a same-file fixture would prove nothing.

mod common;

use common::peer_harness::PeerHarness;
use common::{commit_all, ecp_bin, run_git};
use ecp_core::peer::inbox::{ConcernKindSer, InboxEntry};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Two symbols in SEPARATE files with one Calls edge between them, indexed
/// under a private HOME.
fn index_repo(repo: &Path, home: &Path) {
    fs::write(repo.join("target.rs"), "pub fn target_fn() {}\n").unwrap();
    fs::write(
        repo.join("caller.rs"),
        "mod target;\npub fn caller_one() { target::target_fn(); }\n",
    )
    .unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    commit_all(repo, "init");
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .expect("spawn ecp admin index");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn is_soft_target_fn(e: &InboxEntry) -> bool {
    matches!(
        e,
        InboxEntry::DirtyEvent { kind: ConcernKindSer::Soft, symbol: Some(s), .. }
            if s.name == "target_fn"
    )
}

#[test]
fn peer_touching_a_graph_neighbour_delivers_a_soft_concern() {
    let home = tempfile::tempdir().expect("home");
    let repo = tempfile::tempdir().expect("repo");
    index_repo(repo.path(), home.path());

    // The peers data dir stays the harness tempdir; only the graph lookup
    // needs the indexed repo, and the watcher reaches it through
    // session_meta.source_worktree + HOME.
    let mut h = PeerHarness::new().backed_by(home.path(), repo.path());
    h.spawn_session("alice");
    h.spawn_session("bob");
    std::thread::sleep(Duration::from_millis(800));

    // bob is dirty on the caller; alice then touches the callee, in a file bob
    // has NOT touched → HARD cannot fire, only the graph edge can.
    h.write_dirty("bob", "caller.rs", &[("caller_one", "caller.rs")]);
    std::thread::sleep(Duration::from_millis(500));
    h.write_dirty("alice", "target.rs", &[("target_fn", "target.rs")]);

    let delivered = h.assert_within(Duration::from_secs(5), || {
        h.read_inbox("bob").iter().any(is_soft_target_fn)
    });
    assert!(
        delivered,
        "bob never got a SOFT concern for target_fn; inbox = {:?}",
        h.read_inbox("bob")
    );
}
