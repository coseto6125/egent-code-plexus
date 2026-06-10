//! P4 scale audit: 8 concurrent watcher sessions on one repo root.
//!
//! Verifies the team-scale properties the 2-session tests can't:
//! - every watcher holds its own flock without colliding (8 inotify loops)
//! - a dirty burst from one session fans out to ALL other inboxes
//! - delivery completes within a bounded window at this scale
//!
//! Sized to stay CI-cheap: tiny session dirs in a tempdir, one burst.

mod common;
use common::peer_harness::PeerHarness;
use ecp_core::peer::inbox::InboxEntry;
use std::time::{Duration, Instant};

const N: usize = 8;

fn ids() -> Vec<String> {
    (0..N).map(|i| format!("agent-{i}")).collect()
}

#[test]
fn eight_session_burst_fans_out_to_every_inbox() {
    let mut h = PeerHarness::new();
    let ids = ids();
    for id in &ids {
        h.spawn_session_named(id, Some(&format!("teammate-{id}")));
    }

    // All sessions go dirty on the same symbol → every pair is a HARD overlap.
    // Stagger slightly so watchers see distinct inotify events, then measure
    // how long full N×(N-1) delivery takes.
    let start = Instant::now();
    for id in &ids {
        h.write_dirty(id, "src/auth.rs", &[("verify_token", "src/auth.rs")]);
        std::thread::sleep(Duration::from_millis(20));
    }

    // Each session must hear from every OTHER session at least once.
    let all_delivered = h.assert_within(Duration::from_secs(10), || {
        ids.iter().all(|id| {
            let froms: std::collections::HashSet<String> = h
                .read_inbox(id)
                .iter()
                .filter_map(|e| match e {
                    InboxEntry::DirtyEvent { peer_session, .. } => Some(peer_session.clone()),
                    _ => None,
                })
                .collect();
            froms.len() >= N - 1
        })
    });
    let elapsed = start.elapsed();
    eprintln!("[peers_scale] {N} sessions, full fan-out in {elapsed:?}");
    assert!(
        all_delivered,
        "fan-out incomplete after 10s at {N} sessions — inbox coverage: {:?}",
        ids.iter()
            .map(|id| {
                let n = h
                    .read_inbox(id)
                    .iter()
                    .filter(|e| matches!(e, InboxEntry::DirtyEvent { .. }))
                    .count();
                (id.clone(), n)
            })
            .collect::<Vec<_>>()
    );

    // Names must survive the fan-out (P1 wiring under concurrency).
    let named = h.read_inbox(&ids[0]).iter().any(|e| {
        matches!(e, InboxEntry::DirtyEvent { peer_name: Some(n), .. } if n.starts_with("teammate-"))
    });
    assert!(named, "agent names lost in concurrent dispatch");
}

#[test]
fn watcher_locks_are_per_session_not_global() {
    let mut h = PeerHarness::new();
    for id in ids() {
        h.spawn_session(&id);
    }
    // All N watcher.lock files must be held concurrently — a global lock
    // would serialize watchers and the later spawns would have died.
    let all_alive = h.assert_within(Duration::from_secs(5), || {
        h.watchers
            .iter()
            .all(|s| ecp_core::peer::registry::pid_alive(s.pid))
    });
    assert!(all_alive, "some watcher died — lock collision at scale");
}
