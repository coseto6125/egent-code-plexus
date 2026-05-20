mod common;
use common::peer_harness::PeerHarness;
use std::time::Duration;

#[test]
fn unrelated_symbol_does_not_appear_in_inbox() {
    let mut h = PeerHarness::new();
    h.spawn_session("alice");
    h.spawn_session("bob");
    std::thread::sleep(Duration::from_millis(150));

    h.write_dirty("bob", "src/auth.rs", &[("verify_token", "src/auth.rs")]);
    std::thread::sleep(Duration::from_millis(150));
    h.write_dirty(
        "alice",
        "src/utils/money.rs",
        &[("format_money", "src/utils/money.rs")],
    );

    std::thread::sleep(Duration::from_millis(800));
    let inbox = h.read_inbox("bob");
    assert!(
        inbox.is_empty(),
        "unrelated symbol leaked to inbox: {inbox:?}"
    );
}

#[test]
fn same_symbol_triggers_hard_event() {
    let mut h = PeerHarness::new();
    h.spawn_session("alice");
    h.spawn_session("bob");
    std::thread::sleep(Duration::from_millis(150));

    h.write_dirty("bob", "src/auth.rs", &[("verify_token", "src/auth.rs")]);
    std::thread::sleep(Duration::from_millis(150));
    h.write_dirty("alice", "src/auth.rs", &[("verify_token", "src/auth.rs")]);

    let arrived = h.assert_within(Duration::from_millis(2000), || {
        h.read_inbox("bob").iter().any(|e| {
            matches!(
                e,
                ecp_core::peer::inbox::InboxEntry::DirtyEvent {
                    kind: ecp_core::peer::inbox::ConcernKindSer::Hard,
                    ..
                }
            )
        })
    });
    assert!(arrived, "HARD event missing within 2s");
}
