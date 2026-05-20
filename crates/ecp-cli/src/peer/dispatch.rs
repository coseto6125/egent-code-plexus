//! Bridge: classify peer dirty entry → append InboxEntry to receiver inbox.

use ecp_core::peer::concern::{classify, ConcernResult, ImpactCache};
use ecp_core::peer::inbox::{append_entry, ConcernKindSer, InboxEntry};
use ecp_core::session::overlay::{DirtyEntry, SymbolRef};
use std::io;
use std::path::Path;

pub fn dispatch_peer_dirty_event(
    receiver_session_dir: &Path,
    peer_session: &str,
    peer_pid: u32,
    ts: &str,
    peer_entry: &DirtyEntry,
    my_dirty_symbols: &[SymbolRef],
    impact_cache: &ImpactCache,
) -> io::Result<()> {
    let result = classify(&peer_entry.dirty_symbols, my_dirty_symbols, impact_cache);
    let (kind, symbol, reason) = match result {
        ConcernResult::Hit {
            kind,
            symbol,
            reason,
        } => (kind, symbol, reason),
        ConcernResult::Ignore => return Ok(()),
    };
    let entry = InboxEntry::DirtyEvent {
        ts: ts.to_string(),
        peer_session: peer_session.to_string(),
        peer_pid,
        kind: ConcernKindSer::from(kind),
        symbol,
        reason,
        peer_delta: None,
        your_overlap_range: None,
    };
    let inbox = receiver_session_dir.join("inbox.jsonl");
    append_entry(&inbox, &entry)
}
