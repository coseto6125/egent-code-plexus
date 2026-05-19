use cgn_core::peer::inbox::{
    append_entry, drain, truncate_inbox, ConcernKindSer, InboxEntry,
};
use cgn_core::session::overlay::{SymbolKind, SymbolRef};
use tempfile::tempdir;

fn dirty_event_fixture() -> InboxEntry {
    InboxEntry::DirtyEvent {
        ts: "2026-05-17T00:00:00Z".into(),
        peer_session: "abc12".into(),
        peer_pid: 1234,
        kind: ConcernKindSer::Hard,
        symbol: SymbolRef {
            name: "verify_token".into(),
            kind: SymbolKind::Function,
            file: "src/auth.rs".into(),
            line_start: 1,
            line_end: 10,
        },
        reason: "Both sessions modified verify_token".into(),
        peer_delta: Some("-old\n+new".into()),
        your_overlap_range: Some((5, 7)),
    }
}

#[test]
fn append_then_drain_returns_all_entries() {
    let dir = tempdir().unwrap();
    let inbox = dir.path().join("inbox.jsonl");
    append_entry(&inbox, &dirty_event_fixture()).unwrap();
    append_entry(
        &inbox,
        &InboxEntry::Message {
            ts: "2026-05-17T00:00:01Z".into(),
            msg_id: "m_1".into(),
            from: "abc12".into(),
            to: None,
            reply_to: None,
            body: "hi".into(),
        },
    )
    .unwrap();

    let (entries, new_offset) = drain(&inbox, 0).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(new_offset > 0);

    let (entries2, _) = drain(&inbox, new_offset).unwrap();
    assert!(
        entries2.is_empty(),
        "second drain at watermark sees nothing new"
    );
}

#[test]
fn drain_handles_missing_file_as_empty() {
    let dir = tempdir().unwrap();
    let (entries, off) = drain(&dir.path().join("absent.jsonl"), 0).unwrap();
    assert!(entries.is_empty());
    assert_eq!(off, 0);
}

#[test]
fn drain_resets_offset_when_file_truncated_below_watermark() {
    let dir = tempdir().unwrap();
    let inbox = dir.path().join("inbox.jsonl");
    append_entry(&inbox, &dirty_event_fixture()).unwrap();
    let (_, off) = drain(&inbox, 0).unwrap();
    assert!(off > 0);

    std::fs::write(&inbox, "").unwrap();
    append_entry(&inbox, &dirty_event_fixture()).unwrap();

    let (entries, _) = drain(&inbox, off).unwrap();
    assert_eq!(
        entries.len(),
        1,
        "after external shrink, drain re-reads from offset 0"
    );
}

#[test]
fn truncate_inbox_bumps_gen_so_drain_detects_shrink() {
    let dir = tempdir().unwrap();
    let inbox = dir.path().join("inbox.jsonl");

    append_entry(&inbox, &dirty_event_fixture()).unwrap();
    let (_, watermark) = drain(&inbox, 0).unwrap();
    assert!(watermark > 0);

    truncate_inbox(&inbox).unwrap();
    append_entry(&inbox, &dirty_event_fixture()).unwrap();

    let (entries, _) = drain(&inbox, watermark).unwrap();
    assert_eq!(
        entries.len(),
        1,
        "post-truncate-and-regrow append must be visible to drain at old watermark"
    );
}

#[test]
fn drain_skips_corrupt_line_and_continues() {
    let dir = tempdir().unwrap();
    let inbox = dir.path().join("inbox.jsonl");
    std::fs::write(&inbox, "not valid json\n").unwrap();
    append_entry(&inbox, &dirty_event_fixture()).unwrap();

    let (entries, _) = drain(&inbox, 0).unwrap();
    assert_eq!(entries.len(), 1, "corrupt line skipped, good line returned");
}
