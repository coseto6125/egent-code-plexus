mod common;
use common::peer_harness::PeerHarness;
use std::time::Duration;

#[test]
fn peer_dirty_arrives_in_my_inbox_within_2s() {
    let mut h = PeerHarness::new();
    h.spawn_session("alice");
    h.spawn_session("bob");

    // bob has the same symbol dirty → alice's same-symbol edit should HARD on bob's inbox
    h.write_dirty("bob", "src/auth.rs", &[("verify_token", "src/auth.rs")]);
    std::thread::sleep(Duration::from_millis(150));
    h.write_dirty("alice", "src/auth.rs", &[("verify_token", "src/auth.rs")]);

    let arrived = h.assert_within(Duration::from_millis(2000), || {
        !h.read_inbox("bob").is_empty()
    });
    assert!(
        arrived,
        "bob's inbox empty after 2s — watcher did not dispatch alice's dirty event"
    );
}

/// The LATE writer must also learn about overlaps that existed before its
/// own first dirty write. Edge-triggered dispatch alone misses this: when
/// alice's event arrived, bob's dirty set was still empty → Ignore — and no
/// later peer event re-evaluates. The watcher must rescan peers on
/// self-dirty changes. (Found by the 8-session scale audit: delivery formed
/// a staircase ending at 0 for the last writer.)
#[test]
fn late_writer_learns_existing_peer_overlap() {
    let mut h = PeerHarness::new();
    h.spawn_session("carol");
    h.spawn_session("dave");

    h.write_dirty("carol", "src/auth.rs", &[("verify_token", "src/auth.rs")]);
    std::thread::sleep(Duration::from_millis(150));
    // dave goes dirty AFTER carol's event already passed him by
    h.write_dirty("dave", "src/auth.rs", &[("verify_token", "src/auth.rs")]);

    let arrived = h.assert_within(Duration::from_millis(2000), || {
        !h.read_inbox("dave").is_empty()
    });
    assert!(
        arrived,
        "dave (late writer) never learned about carol's pre-existing overlap"
    );
}
