//! Render drained InboxEntry batches into a Claude Code hook payload.
//!
//! 4 KB hard cap. What gets trimmed is ordered by whether it can be recovered:
//! SOFT first, then HARD detail, and messages last and never entirely. A
//! concern is re-derivable — the peer's manifest is still on disk and the
//! watcher raises it again on the next write — but a peer message exists only
//! in this batch, and the caller drops the inbox once this returns. Trimming a
//! message away is therefore the one loss nothing can undo.

use ecp_core::peer::inbox::{ConcernKindSer, InboxEntry};
use std::collections::HashSet;
use std::fmt::Write;

const PAYLOAD_CAP_BYTES: usize = 4096;
/// Room kept for the "N more held back" trailer, which is only written when
/// something did not fit — so it is reserved only on the pass that needs it.
const TRAILER_RESERVE: usize = 176;
const HARD_DELTA_LOC_CAP: usize = 30;

/// Returns the payload and the indices of `entries` it actually represented.
/// The caller removes exactly those; anything that did not fit stays in the
/// inbox for the next hook rather than being cleared unseen.
pub fn render_payload(entries: &[InboxEntry]) -> (String, HashSet<usize>) {
    if entries.is_empty() {
        return (String::new(), HashSet::new());
    }
    // A repeated (peer, file, name, kind) concern is the same concern: the
    // watcher's self-dirty rescan and repeated peer saves both re-raise it.
    // Only the LAST occurrence is rendered — it carries the freshest delta —
    // and the earlier ones count as represented, so they are consumed too.
    // HARD's identity is the file, so its witness declaration is not part of
    // the key; SOFT keys on the declaration, which is what it claims.
    let mut seen: HashSet<(&str, &str, &str, ConcernKindSer)> = HashSet::new();
    let mut superseded: HashSet<usize> = HashSet::new();
    let mut hard: Vec<(usize, &InboxEntry)> = Vec::new();
    let mut soft: Vec<(usize, &InboxEntry)> = Vec::new();
    let mut msgs: Vec<(usize, &InboxEntry)> = Vec::new();
    for (i, e) in entries.iter().enumerate().rev() {
        match e {
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
                if !seen.insert((peer_session.as_str(), symbol.file.as_str(), witness, *kind)) {
                    superseded.insert(i);
                    continue;
                }
                match kind {
                    ConcernKindSer::Hard => hard.push((i, e)),
                    ConcernKindSer::Soft => soft.push((i, e)),
                }
            }
            InboxEntry::Message { .. } => msgs.push((i, e)),
        }
    }
    hard.reverse();
    soft.reverse();
    msgs.reverse();

    // Plans in order of what each sacrifices; the first that represents
    // everything wins. Concerns are re-derivable — the peer's manifest is on
    // disk and the watcher raises them again — so they are given up before a
    // message, which exists only in this batch.
    let total = hard.len() + soft.len() + msgs.len();
    let ladder = |cap: usize| {
        let mut best = compose(&hard, &soft, &msgs, ATTEMPTS[0], cap);
        for plan in &ATTEMPTS[1..] {
            if best.1.len() == total {
                break;
            }
            let attempt = compose(&hard, &soft, &msgs, *plan, cap);
            if attempt.1.len() > best.1.len() {
                best = attempt;
            }
        }
        best
    };
    let mut best = ladder(PAYLOAD_CAP_BYTES);
    if best.1.len() < total {
        // Something is being held back, so the trailer will be written — redo
        // the ladder with its room reserved rather than overflowing the cap.
        best = ladder(PAYLOAD_CAP_BYTES.saturating_sub(TRAILER_RESERVE));
    }
    let (mut buf, mut shown) = best;
    let unseen = total - shown.len();
    if unseen > 0 {
        let _ = writeln!(
            buf,
            "\n[ecp peers] {unseen} more held back by the 4KB cap — they stay in the inbox for the next turn (`ecp peers inbox` to read now)"
        );
    }
    shown.extend(superseded);
    (buf, shown)
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
        soft: true,
        hard_detail: false,
        body_budget: 500,
    },
    Plan {
        soft: true,
        hard_detail: false,
        body_budget: 240,
    },
    Plan {
        soft: true,
        hard_detail: false,
        body_budget: 60,
    },
    Plan {
        soft: true,
        hard_detail: false,
        body_budget: 0,
    },
];

/// Build a payload under `plan`, appending an item only while it still fits.
/// Nothing is ever cut mid-item, so every index reported was rendered whole.
fn compose(
    hard: &[(usize, &InboxEntry)],
    soft: &[(usize, &InboxEntry)],
    msgs: &[(usize, &InboxEntry)],
    plan: Plan,
    cap: usize,
) -> (String, HashSet<usize>) {
    let mut buf = String::new();
    let mut shown = HashSet::new();
    // Messages first: they are the only content with no other copy once the
    // caller consumes them, so they claim the budget before any concern does.
    let section = |buf: &mut String,
                   shown: &mut HashSet<usize>,
                   header: String,
                   items: &[(usize, &InboxEntry)],
                   render: &dyn Fn(&mut String, &InboxEntry)| {
        if items.is_empty() {
            return;
        }
        let mut pending = header;
        for (i, e) in items {
            let mut block = String::new();
            render(&mut block, e);
            if buf.len() + pending.len() + block.len() > cap {
                break;
            }
            buf.push_str(&pending);
            pending = String::new();
            buf.push_str(&block);
            shown.insert(*i);
        }
    };
    let budget = plan.body_budget;
    section(
        &mut buf,
        &mut shown,
        format!(
            "[ecp peers] {} new message{} Ƀ\n",
            msgs.len(),
            if msgs.len() == 1 { "" } else { "s" }
        ),
        msgs,
        &move |b, e| render_message(b, e, budget),
    );
    let detail = plan.hard_detail;
    section(
        &mut buf,
        &mut shown,
        format!(
            "\n[ecp peers] HARD overlap ({} event{})\n",
            hard.len(),
            if hard.len() == 1 { "" } else { "s" }
        ),
        hard,
        &move |b, e| {
            if detail {
                render_hard(b, e)
            } else {
                render_soft_one_line(b, e)
            }
        },
    );
    if plan.soft {
        section(
            &mut buf,
            &mut shown,
            format!(
                "\n[ecp peers] SOFT overlap ({} event{})\n",
                soft.len(),
                if soft.len() == 1 { "" } else { "s" }
            ),
            soft,
            &render_soft_one_line,
        );
    }
    (buf, shown)
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
