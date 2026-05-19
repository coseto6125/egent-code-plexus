//! Render drained InboxEntry batches into a Claude Code hook payload.
//! 4 KB hard cap; HARD kept, SOFT trimmed first when over.

use graph_nexus_core::peer::inbox::{ConcernKindSer, InboxEntry};
use std::fmt::Write;

const PAYLOAD_CAP_BYTES: usize = 4096;
const HARD_DELTA_LOC_CAP: usize = 30;
const SOFT_EVENTS_DEFAULT_CAP: usize = 10;

pub fn render_payload(entries: &[InboxEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let (mut hard, mut soft, mut msgs) = (Vec::new(), Vec::new(), Vec::new());
    for e in entries {
        match e {
            InboxEntry::DirtyEvent {
                kind: ConcernKindSer::Hard,
                ..
            } => hard.push(e),
            InboxEntry::DirtyEvent {
                kind: ConcernKindSer::Soft,
                ..
            } => soft.push(e),
            InboxEntry::Message { .. } => msgs.push(e),
        }
    }
    let mut buf = String::new();
    if !hard.is_empty() {
        let _ = writeln!(
            buf,
            "[gnx peers] HARD overlap ({} event{})",
            hard.len(),
            if hard.len() == 1 { "" } else { "s" }
        );
        for e in &hard {
            render_hard(&mut buf, e);
        }
    }
    if !soft.is_empty() {
        let cap = SOFT_EVENTS_DEFAULT_CAP.min(soft.len());
        let _ = writeln!(
            buf,
            "\n[gnx peers] SOFT overlap ({} event{})",
            soft.len(),
            if soft.len() == 1 { "" } else { "s" }
        );
        for e in soft.iter().take(cap) {
            render_soft_one_line(&mut buf, e);
        }
        if soft.len() > cap {
            let _ = writeln!(
                buf,
                "  ... +{} more, run `gnx peers status`",
                soft.len() - cap
            );
        }
    }
    if !msgs.is_empty() {
        let _ = writeln!(
            buf,
            "\n[gnx peers] {} new message{} Ƀ",
            msgs.len(),
            if msgs.len() == 1 { "" } else { "s" }
        );
        for e in &msgs {
            render_message(&mut buf, e);
        }
    }
    enforce_cap(buf, &hard)
}

fn render_hard(buf: &mut String, e: &InboxEntry) {
    if let InboxEntry::DirtyEvent {
        peer_session,
        peer_pid,
        ts,
        symbol,
        reason,
        peer_delta,
        your_overlap_range,
        ..
    } = e
    {
        let _ = writeln!(buf, "  Peer:   {peer_session} (pid {peer_pid})");
        let _ = writeln!(buf, "  When:   {ts}");
        let _ = writeln!(
            buf,
            "  Symbol: {} · {:?} · {}:{}-{}",
            symbol.name, symbol.kind, symbol.file, symbol.line_start, symbol.line_end
        );
        let _ = writeln!(buf, "  Reason: {reason}");
        if let Some(d) = peer_delta {
            let lines: Vec<&str> = d.lines().take(HARD_DELTA_LOC_CAP).collect();
            let _ = writeln!(buf, "  Peer delta:");
            for l in &lines {
                let _ = writeln!(buf, "    {l}");
            }
            if d.lines().count() > HARD_DELTA_LOC_CAP {
                let _ = writeln!(
                    buf,
                    "    ... (truncated, see `gnx peers diff {peer_session} {}`)",
                    symbol.name
                );
            }
        }
        if let Some((s, end)) = your_overlap_range {
            let _ = writeln!(buf, "  Your overlap range: L{s}-{end}");
        }
        let _ = writeln!(
            buf,
            "  Suggest: Review peer delta before saving conflicting edits"
        );
    }
}

fn render_soft_one_line(buf: &mut String, e: &InboxEntry) {
    if let InboxEntry::DirtyEvent {
        peer_session,
        ts,
        symbol,
        ..
    } = e
    {
        let _ = writeln!(
            buf,
            "  · {} ({:?}, {}:{}) by {peer_session} ({ts})",
            symbol.name, symbol.kind, symbol.file, symbol.line_start
        );
    }
}

fn render_message(buf: &mut String, e: &InboxEntry) {
    if let InboxEntry::Message {
        msg_id,
        from,
        to,
        reply_to,
        body,
        ts,
        ..
    } = e
    {
        let to_part = match to {
            Some(t) => format!(" → {t}"),
            None => " → all".into(),
        };
        let reply_part = reply_to
            .as_ref()
            .map(|r| format!(" (reply to {r})"))
            .unwrap_or_default();
        let truncated: String = body.chars().take(500).collect();
        let _ = writeln!(buf, "  [{msg_id}] {from}{to_part}{reply_part} ({ts})");
        let _ = writeln!(buf, "    {truncated}");
    }
}

fn enforce_cap(mut buf: String, hard: &[&InboxEntry]) -> String {
    if buf.len() <= PAYLOAD_CAP_BYTES {
        return buf;
    }
    buf.clear();
    let _ = writeln!(
        &mut buf,
        "[gnx peers] HARD overlap ({}) — payload trimmed to fit 4KB cap",
        hard.len()
    );
    for e in hard {
        render_hard(&mut buf, e);
        if buf.len() > PAYLOAD_CAP_BYTES {
            buf.truncate(PAYLOAD_CAP_BYTES.saturating_sub(80));
            buf.push_str("\n... (truncated)\n");
            break;
        }
    }
    buf
}
