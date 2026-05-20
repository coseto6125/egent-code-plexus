mod common;
use common::peer_harness::PeerHarness;
use std::time::Duration;

#[test]
fn changing_self_dirty_invalidates_impact_cache_eventually() {
    let mut h = PeerHarness::new();
    h.spawn_session("alice");
    h.spawn_session("bob");
    std::thread::sleep(Duration::from_millis(1000));

    // bob has no dirty → alice's event should be IGNORE
    h.write_dirty("alice", "src/a.rs", &[("foo", "src/a.rs")]);
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        h.read_inbox("bob").is_empty(),
        "bob got events before he had any dirty"
    );

    // bob adds the same symbol → alice's next event should produce HARD
    h.write_dirty("bob", "src/a.rs", &[("foo", "src/a.rs")]);
    std::thread::sleep(Duration::from_millis(500));
    h.write_dirty("alice", "src/a.rs", &[("foo", "src/a.rs")]);

    let got = h.assert_within(Duration::from_millis(2000), || {
        !h.read_inbox("bob").is_empty()
    });
    assert!(
        got,
        "cache invalidation did not propagate — bob still empty"
    );
}
