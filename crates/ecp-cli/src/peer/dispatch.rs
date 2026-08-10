//! Bridge: classify peer dirty entry → append InboxEntry to receiver inbox.

use ecp_core::peer::concern::{classify, ConcernResult, ImpactCache};
use ecp_core::peer::inbox::{append_entry, ConcernKindSer, InboxEntry};
use ecp_core::session::overlay::{DirtyEntry, SymbolRef};
use std::io;
use std::path::Path;

#[allow(clippy::too_many_arguments)]
pub fn dispatch_peer_dirty_event(
    receiver_session_dir: &Path,
    peer_session: &str,
    peer_pid: u32,
    peer_name: Option<&str>,
    ts: &str,
    peer_file: &str,
    peer_entry: &DirtyEntry,
    my_dirty_files: &[String],
    my_dirty_symbols: &[SymbolRef],
    impact_cache: &ImpactCache,
) -> io::Result<()> {
    let result = classify(
        peer_file,
        &peer_entry.dirty_symbols,
        my_dirty_files,
        my_dirty_symbols,
        impact_cache,
    );
    let (kind, file, symbol, reason) = match result {
        ConcernResult::Hit {
            kind,
            file,
            symbol,
            reason,
        } => (kind, file, symbol, reason),
        ConcernResult::Ignore => return Ok(()),
    };
    let entry = InboxEntry::DirtyEvent {
        ts: ts.to_string(),
        peer_session: peer_session.to_string(),
        peer_pid,
        peer_name: peer_name.map(str::to_string),
        kind: ConcernKindSer::from(kind),
        file,
        symbol,
        reason,
        peer_delta: None,
        your_overlap_range: None,
    };
    let inbox = receiver_session_dir.join("inbox.jsonl");
    // A concern is re-derivable — the peer's manifest is on disk and the next
    // write raises it again — so a failed append is survivable, but it must not
    // be silent: the watcher's fail-open loop would otherwise swallow it.
    if let Err(e) = append_entry(&inbox, &entry) {
        tracing::warn!(error = %e, peer = peer_session, "could not deliver concern to inbox");
        return Err(e);
    }
    Ok(())
}
