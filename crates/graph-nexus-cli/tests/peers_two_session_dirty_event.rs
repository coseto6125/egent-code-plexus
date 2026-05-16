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
