use ecp_cli::peer::render::render_payload;
use ecp_core::peer::inbox::{ConcernKindSer, InboxEntry};
use ecp_core::session::overlay::{SymbolKind, SymbolRef};

fn dirty_hard() -> InboxEntry {
    InboxEntry::DirtyEvent {
        ts: "2026-05-17T00:00:30Z".into(),
        peer_session: "abc12".into(),
        peer_pid: 1234,
        peer_name: None,
        kind: ConcernKindSer::Hard,
        file: "src/auth.rs".into(),
        symbol: None,
        reason: "Both sessions have src/auth.rs in their overlay".into(),
        peer_delta: Some("-old\n+new".into()),
        your_overlap_range: Some((45, 50)),
    }
}

#[test]
fn empty_input_renders_empty_string() {
    assert!(render_payload(&[]).0.is_empty());
}

#[test]
fn single_hard_event_renders_header_and_delta() {
    let out = render_payload(&[dirty_hard()]).0;
    assert!(out.contains("HARD overlap"), "missing HARD header: {out}");
    assert!(
        out.contains("src/auth.rs"),
        "missing the shared file: {out}"
    );
    assert!(out.contains("-old"));
    assert!(out.contains("+new"));
    assert!(out.contains("Suggest"));
}

/// HARD knows the file and nothing finer. Printing a declaration with exact
/// lines above a reason that says which declarations changed is unknown puts
/// the two lines in contradiction, and an agent believes the specific one.
#[test]
fn hard_render_names_no_declaration_and_no_line_range() {
    let out = render_payload(&[dirty_hard()]).0;
    assert!(
        !out.contains("Symbol:"),
        "HARD must not present a symbol field: {out}"
    );
    assert!(
        !out.contains("42-58"),
        "a line range asserts a located edit: {out}"
    );
    assert!(out.contains("File:"), "expected a file line: {out}");
}

#[test]
fn message_event_renders_msg_id_body_and_beta_marker() {
    let msg = InboxEntry::Message {
        ts: "2026-05-17T00:00:10Z".into(),
        msg_id: "m_001".into(),
        from: "abc12".into(),
        from_name: None,
        to: None,
        reply_to: None,
        body: "hello peers".into(),
    };
    let out = render_payload(&[msg]).0;
    assert!(out.contains("[m_001]"));
    assert!(out.contains("hello peers"));
    assert!(
        out.contains("Ƀ"),
        "messages section must carry the beta marker"
    );
}

#[test]
fn hard_payload_prefers_agent_name_keeps_session_id() {
    let named = match dirty_hard() {
        InboxEntry::DirtyEvent {
            ts,
            peer_session,
            peer_pid,
            kind,
            symbol,
            reason,
            peer_delta,
            your_overlap_range,
            file,
            ..
        } => InboxEntry::DirtyEvent {
            ts,
            peer_session,
            peer_pid,
            peer_name: Some("rust-parser".into()),
            kind,
            file,
            symbol,
            reason,
            peer_delta,
            your_overlap_range,
        },
        other => panic!("dirty_hard returned wrong variant: {other:?}"),
    };
    let out = render_payload(&[named]).0;
    assert!(
        out.contains("Peer:   rust-parser (session abc12, pid 1234)"),
        "named peer line wrong: {out}"
    );
    assert!(
        out.contains(r#"coordinate: SendMessage to "rust-parser""#),
        "named HARD must carry an actionable coordinate hint: {out}"
    );

    let anon = render_payload(&[dirty_hard()]).0;
    assert!(
        anon.contains("Peer:   abc12 (pid 1234)"),
        "anon peer line wrong: {anon}"
    );
    assert!(
        !anon.contains("SendMessage"),
        "no speculative hint when the peer has no team name: {anon}"
    );
}

#[test]
fn message_renders_from_name_when_present() {
    let msg = InboxEntry::Message {
        ts: "t".into(),
        msg_id: "m_002".into(),
        from: "abc12".into(),
        from_name: Some("graph-lead".into()),
        to: None,
        reply_to: None,
        body: "ping".into(),
    };
    let out = render_payload(&[msg]).0;
    assert!(out.contains("graph-lead"), "from_name not shown: {out}");
}

#[test]
fn old_inbox_line_without_peer_name_still_parses() {
    let line = r#"{"type":"dirty_event","ts":"t","peer_session":"s1","peer_pid":1,"kind":"hard","symbol":{"name":"f","kind":"function","file":"a.rs","line_start":1,"line_end":2},"reason":"r","peer_delta":null,"your_overlap_range":null}"#;
    let e: InboxEntry = serde_json::from_str(line).expect("pre-agent_name line must parse");
    match e {
        InboxEntry::DirtyEvent { peer_name, .. } => assert_eq!(peer_name, None),
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn enforces_4kb_cap_with_hard_priority() {
    let mut bulk: Vec<InboxEntry> = Vec::new();
    for i in 0..200 {
        bulk.push(InboxEntry::DirtyEvent {
            ts: "ts".into(),
            peer_session: format!("p{i}"),
            peer_pid: 1,
            peer_name: None,
            kind: ConcernKindSer::Soft,
            file: "src/x.rs".into(),
            symbol: Some(SymbolRef {
                name: format!("sym_{i}"),
                kind: SymbolKind::Function,
                file: "src/x.rs".into(),
                line_start: 1,
                line_end: 2,
            }),
            reason: "neighbor".into(),
            peer_delta: None,
            your_overlap_range: None,
        });
    }
    bulk.insert(0, dirty_hard());
    let out = render_payload(&bulk).0;
    assert!(out.len() <= 4096, "payload exceeds 4 KB cap: {}", out.len());
    assert!(out.contains("HARD overlap"), "HARD must survive trimming");
}

#[test]
fn duplicate_dirty_events_same_peer_symbol_render_once() {
    // The watcher's self-dirty rescan can re-dispatch an overlap a peer
    // event already delivered; the payload must not show the same
    // (peer, symbol) concern twice.
    let out = render_payload(&[dirty_hard(), dirty_hard()]).0;
    assert!(
        out.contains("HARD overlap (1 event)"),
        "duplicates must collapse: {out}"
    );
    assert_eq!(
        out.matches("Peer:   abc12").count(),
        1,
        "one Peer block expected: {out}"
    );
}
