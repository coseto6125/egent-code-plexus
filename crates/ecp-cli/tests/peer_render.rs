use ecp_cli::peer::render::render_payload;
use ecp_core::peer::inbox::{ConcernKindSer, InboxEntry};
use ecp_core::session::overlay::{SymbolKind, SymbolRef};

fn dirty_hard() -> InboxEntry {
    InboxEntry::DirtyEvent {
        ts: "2026-05-17T00:00:30Z".into(),
        peer_session: "abc12".into(),
        peer_pid: 1234,
        kind: ConcernKindSer::Hard,
        symbol: SymbolRef {
            name: "verify_token".into(),
            kind: SymbolKind::Function,
            file: "src/auth.rs".into(),
            line_start: 42,
            line_end: 58,
        },
        reason: "Both sessions modified verify_token".into(),
        peer_delta: Some("-old\n+new".into()),
        your_overlap_range: Some((45, 50)),
    }
}

#[test]
fn empty_input_renders_empty_string() {
    assert!(render_payload(&[]).is_empty());
}

#[test]
fn single_hard_event_renders_header_and_delta() {
    let out = render_payload(&[dirty_hard()]);
    assert!(out.contains("HARD overlap"), "missing HARD header: {out}");
    assert!(out.contains("verify_token"));
    assert!(out.contains("src/auth.rs:42-58"));
    assert!(out.contains("-old"));
    assert!(out.contains("+new"));
    assert!(out.contains("Suggest"));
}

#[test]
fn message_event_renders_msg_id_body_and_beta_marker() {
    let msg = InboxEntry::Message {
        ts: "2026-05-17T00:00:10Z".into(),
        msg_id: "m_001".into(),
        from: "abc12".into(),
        to: None,
        reply_to: None,
        body: "hello peers".into(),
    };
    let out = render_payload(&[msg]);
    assert!(out.contains("[m_001]"));
    assert!(out.contains("hello peers"));
    assert!(
        out.contains("Ƀ"),
        "messages section must carry the beta marker"
    );
}

#[test]
fn enforces_4kb_cap_with_hard_priority() {
    let mut bulk: Vec<InboxEntry> = Vec::new();
    for i in 0..200 {
        bulk.push(InboxEntry::DirtyEvent {
            ts: "ts".into(),
            peer_session: format!("p{i}"),
            peer_pid: 1,
            kind: ConcernKindSer::Soft,
            symbol: SymbolRef {
                name: format!("sym_{i}"),
                kind: SymbolKind::Function,
                file: "src/x.rs".into(),
                line_start: 1,
                line_end: 2,
            },
            reason: "neighbor".into(),
            peer_delta: None,
            your_overlap_range: None,
        });
    }
    bulk.insert(0, dirty_hard());
    let out = render_payload(&bulk);
    assert!(out.len() <= 4096, "payload exceeds 4 KB cap: {}", out.len());
    assert!(out.contains("HARD overlap"), "HARD must survive trimming");
}
