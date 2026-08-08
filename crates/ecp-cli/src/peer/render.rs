//! Render drained InboxEntry batches into a Claude Code hook payload.
//!
//! 4 KB hard cap. What gets trimmed is ordered by whether it can be recovered:
//! SOFT first, then HARD detail, and messages last and never entirely. A
//! concern is re-derivable — the peer's manifest is still on disk and the
//! watcher raises it again on the next write — but a peer message exists only
//! in this batch, and the caller drops the inbox once this returns. Trimming a
//! message away is therefore the one loss nothing can undo.

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
    // HARD's identity is the shared FILE, so its witness declaration is not
    // part of the key: a peer that gains a declaration between two events
    // would otherwise render as two concerns about one file. SOFT does key on
    // the declaration, since that is what it actually claims.
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
            } => {
                let witness = match kind {
                    ConcernKindSer::Hard => "",
                    ConcernKindSer::Soft => symbol.name.as_str(),
                };
                seen.insert((peer_session.as_str(), symbol.file.as_str(), witness, *kind))
            }
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
    // Attempts in order of what each sacrifices, stopping at the first that
    // fits: full detail, then SOFT dropped, then HARD reduced to one line each,
    // then message bodies squeezed. Messages always survive at least as an id
    // and sender, because nothing else holds a copy after this returns.
    let last = ATTEMPTS.len() - 1;
    for (attempt, plan) in ATTEMPTS.iter().enumerate() {
        let buf = compose(&hard, &soft, &msgs, *plan);
        if buf.len() <= PAYLOAD_CAP_BYTES {
            return buf;
        }
        if attempt == last {
            // Even id-and-sender lines overflow: clamp, and say how much of the
            // batch the agent is not seeing rather than cutting silently.
            let mut clamped = buf;
            clamped.truncate(floor_char_boundary(
                &clamped,
                PAYLOAD_CAP_BYTES.saturating_sub(96),
            ));
            let _ = writeln!(
                clamped,
                "\n... batch too large for the 4KB cap ({} concern(s), {} message(s)) — `ecp peers inbox`",
                hard.len() + soft.len(),
                msgs.len()
            );
            return clamped;
        }
    }
    unreachable!("ATTEMPTS is non-empty")
}

#[derive(Clone, Copy)]
struct Plan {
    soft: bool,
    hard_detail: bool,
    body_budget: usize,
}

const ATTEMPTS: &[Plan] = &[
    Plan {
        soft: true,
        hard_detail: true,
        body_budget: 500,
    },
    Plan {
        soft: false,
        hard_detail: true,
        body_budget: 500,
    },
    Plan {
        soft: false,
        hard_detail: false,
        body_budget: 240,
    },
    Plan {
        soft: false,
        hard_detail: false,
        body_budget: 60,
    },
    Plan {
        soft: false,
        hard_detail: false,
        body_budget: 0,
    },
];

fn compose(hard: &[&InboxEntry], soft: &[&InboxEntry], msgs: &[&InboxEntry], plan: Plan) -> String {
    let body_budget = plan.body_budget;
    let mut buf = String::new();
    if !hard.is_empty() {
        let _ = writeln!(
            buf,
            "[ecp peers] HARD overlap ({} event{})",
            hard.len(),
            if hard.len() == 1 { "" } else { "s" }
        );
        for e in hard {
            if plan.hard_detail {
                render_hard(&mut buf, e);
            } else {
                render_soft_one_line(&mut buf, e);
            }
        }
    }
    if plan.soft && !soft.is_empty() {
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
    } else if !soft.is_empty() {
        let _ = writeln!(
            buf,
            "\n[ecp peers] SOFT overlap ({}) omitted to fit the 4KB cap — `ecp peers status`",
            soft.len()
        );
    }
    if !msgs.is_empty() {
        let _ = writeln!(
            buf,
            "\n[ecp peers] {} new message{} Ƀ",
            msgs.len(),
            if msgs.len() == 1 { "" } else { "s" }
        );
        for e in msgs {
            render_message(&mut buf, e, body_budget);
        }
    }
    buf
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
            "  Suggest: Review the peer's version of this file before saving over it"
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

fn render_message(buf: &mut String, e: &InboxEntry, body_budget: usize) {
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
        let truncated: String = body.chars().take(body_budget).collect();
        let elided = body
            .chars()
            .count()
            .saturating_sub(truncated.chars().count());
        let sender = from_name.as_deref().unwrap_or(from);
        let _ = writeln!(buf, "  [{msg_id}] {sender}{to_part}{reply_part} ({ts})");
        if elided > 0 {
            let _ = writeln!(buf, "    {truncated}… (+{elided} chars)");
        } else {
            let _ = writeln!(buf, "    {truncated}");
        }
    }
}

/// Last-resort clamp for the final attempt. `String::truncate` panics on a
/// non-boundary index and message bodies carry arbitrary user text, so a
/// multi-byte character straddling the cap would take the hook down.
fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}
