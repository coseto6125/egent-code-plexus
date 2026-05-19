mod common;
use common::peer_harness::PeerHarness;
use std::time::Duration;

#[test]
fn say_targeted_delivers_to_inbox() {
    let mut h = PeerHarness::new();
    h.spawn_session("alice");
    h.spawn_session("bob");
    std::thread::sleep(Duration::from_millis(100)); // let watchers boot

    let out = h.say("alice", Some("bob"), "ack on auth refactor");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let arrived = h.assert_within(Duration::from_millis(1000), || {
        h.read_inbox("bob").iter().any(|e| {
            matches!(
                e,
                cgn_core::peer::inbox::InboxEntry::Message { body, .. }
                    if body == "ack on auth refactor"
            )
        })
    });
    assert!(arrived, "bob inbox missing targeted message");
}

#[test]
fn say_broadcast_reaches_all_alive_peers() {
    let mut h = PeerHarness::new();
    h.spawn_session("alice");
    h.spawn_session("bob");
    h.spawn_session("carol");
    std::thread::sleep(Duration::from_millis(100));

    let out = h.say("alice", None, "hello team");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    for sid in ["bob", "carol"] {
        let got = h.assert_within(Duration::from_millis(1000), || {
            h.read_inbox(sid).iter().any(|e| {
                matches!(
                    e,
                    cgn_core::peer::inbox::InboxEntry::Message { body, .. }
                        if body == "hello team"
                )
            })
        });
        assert!(got, "{sid} did not receive broadcast");
    }
}
