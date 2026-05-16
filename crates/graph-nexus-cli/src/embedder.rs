//! Process-shared embedder accessor.
//!
//! `Embedder::new()` is a 1–2s cold start (after the ~1.2 GB model is
//! cached on disk). Hooks fork a fresh process per Claude Code tool
//! call, so this `OnceLock` does NOT help the hook path — but it does
//! deduplicate cold-start across rayon workers in a single multi-repo
//! `gnx search` invocation.

use graph_nexus_analyzer::embeddings::Embedder;
use graph_nexus_core::GnxError;
use std::sync::OnceLock;

/// Returns a process-shared `&Embedder` initialised on first call.
/// On init failure (no model + offline, ONNX runtime hiccup) returns
/// `GnxError::InvalidArgument` carrying the underlying error string so
/// callers can `?`-fallback to BM25 cleanly.
pub fn get_embedder() -> Result<&'static Embedder, GnxError> {
    static CELL: OnceLock<Result<Embedder, String>> = OnceLock::new();
    let slot = CELL.get_or_init(|| Embedder::new().map_err(|e| e.to_string()));
    slot.as_ref()
        .map_err(|e| GnxError::InvalidArgument(format!("embedder init: {e}")))
}
