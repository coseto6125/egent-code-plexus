//! Render drained InboxEntry batches into a Claude Code hook payload.
//! 4 KB hard cap; HARD kept, SOFT trimmed first when over.

use ecp_core::peer::inbox::{ConcernKindSer, InboxEntry};
use std::fmt::Write;

const PAYLOAD_CAP_BYTES: usize = 4096;
const HARD_DELTA_LOC_CAP: usize = 30;
const SOFT_EVENTS_DEFAULT_CAP: usize = 10;

pub fn render_payload(entries: &[InboxEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    // The watcher's self-dirty rescan (and repeated peer re-saves) can
    // deliver the same (peer, symbol, kind) concern more than once between
    // drains. Keep only the LAST occurrence — it carries the freshest
    // peer_delta — by scanning in reverse with a seen-set.
    // Keyed on the symbol's FILE as well as its name: a peer concerning us
    // about `run` in two different files is two distinct concerns, and a
    // name-only key would render only the last one.
    let mut seen: std::collections::HashSet<(&str, &str, &str, ConcernKindSer)> =
        std::collections::HashSet::new();
    let mut deduped: Vec<&InboxEntry> = entries
        .iter()
        .rev()
        .filter(|e| match e {
            InboxEntry::DirtyEvent {
                peer_session,
                symbol,
                kind,
                ..
            } => seen.insert((
                peer_session.as_str(),
                symbol.file.as_str(),
                symbol.name.as_str(),
                *kind,
            )),
            InboxEntry::Message { .. } => true,
        })
        .collect();
    deduped.reverse();

    let (mut hard, mut soft, mut msgs) = (Vec::new(), Vec::new(), Vec::new());
    for e in deduped {
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
            "[ecp peers] HARD overlap ({} event{})",
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
            "\n[ecp peers] SOFT overlap ({} event{})",
            soft.len(),
            if soft.len() == 1 { "" } else { "s" }
        );
        for e in soft.iter().take(cap) {
            render_soft_one_line(&mut buf, e);
        }
        if soft.len() > cap {
            let _ = writeln!(
                buf,
                "  ... +{} more, run `ecp peers status`",
                soft.len() - cap
            );
        }
    }
    if !msgs.is_empty() {
        let _ = writeln!(
            buf,
            "\n[ecp peers] {} new message{} Ƀ",
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
        peer_name,
        ts,
        symbol,
        reason,
        peer_delta,
        your_overlap_range,
        ..
    } = e
    {
        let _ = match peer_name {
            Some(n) => writeln!(
                buf,
                "  Peer:   {n} (session {peer_session}, pid {peer_pid})"
            ),
            None => writeln!(buf, "  Peer:   {peer_session} (pid {peer_pid})"),
        };
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
                    "    ... (truncated, see `ecp peers diff {peer_session} {}`)",
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
        // Actionable only with a team name — session ids aren't addressable
        // by the harness's SendMessage, so no hint rather than a dead one.
        if let Some(n) = peer_name {
            let _ = writeln!(buf, "  \u{2192} coordinate: SendMessage to \"{n}\"");
        }
    }
}

fn render_soft_one_line(buf: &mut String, e: &InboxEntry) {
    if let InboxEntry::DirtyEvent {
        peer_session,
        peer_name,
        ts,
        symbol,
        ..
    } = e
    {
        let by = peer_name.as_deref().unwrap_or(peer_session);
        let _ = writeln!(
            buf,
            "  · {} ({:?}, {}:{}) by {by} ({ts})",
            symbol.name, symbol.kind, symbol.file, symbol.line_start
        );
    }
}

fn render_message(buf: &mut String, e: &InboxEntry) {
    if let InboxEntry::Message {
        msg_id,
        from,
        from_name,
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
        let sender = from_name.as_deref().unwrap_or(from);
        let _ = writeln!(buf, "  [{msg_id}] {sender}{to_part}{reply_part} ({ts})");
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
        "[ecp peers] HARD overlap ({}) — payload trimmed to fit 4KB cap",
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
