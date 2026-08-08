//! SOFT concerns end-to-end: a peer editing a graph neighbour of our dirty
//! symbols must reach our inbox.
//!
//! The watcher classified SOFT against an always-empty impact cache until the
//! graph was wired in, so only HARD (same-symbol) overlaps ever fired. Here
//! bob is dirty on `caller_one` and alice touches `target_fn`, which bob only
//! reaches through a Calls edge — no name intersection, so a HARD-only watcher
//! stays silent.

mod common;

use common::peer_harness::PeerHarness;
use common::{commit_all, ecp_bin, run_git};
use ecp_core::peer::inbox::{ConcernKindSer, InboxEntry};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Two symbols, one Calls edge between them, indexed under a private HOME.
fn index_repo(repo: &Path, home: &Path) {
    fs::write(
        repo.join("lib.rs"),
        "pub fn target_fn() {}\npub fn caller_one() { target_fn(); }\n",
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
        InboxEntry::DirtyEvent { kind: ConcernKindSer::Soft, symbol, .. }
            if symbol.name == "target_fn"
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

    // bob is dirty on the caller; alice then touches the callee. No name
    // overlap → HARD cannot fire, only the graph edge can.
    h.write_dirty("bob", "lib.rs", &[("caller_one", "lib.rs")]);
    std::thread::sleep(Duration::from_millis(500));
    h.write_dirty("alice", "lib.rs", &[("target_fn", "lib.rs")]);

    let delivered = h.assert_within(Duration::from_secs(5), || {
        h.read_inbox("bob").iter().any(is_soft_target_fn)
    });
    assert!(
        delivered,
        "bob never got a SOFT concern for target_fn; inbox = {:?}",
        h.read_inbox("bob")
    );
}
