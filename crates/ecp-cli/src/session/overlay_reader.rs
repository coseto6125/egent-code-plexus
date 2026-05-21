//! L1 overlay reader: materialises the on-disk fragment set
//! (`dirty_files.json` + `graph_overlay/<id>.bin` files) into an
//! `ecp_core::session::Overlay` that `merge_archived` can consume directly.
//!
//! Fragment → Node conversion:
//! - `uid`:  `ecp_core::uid::compute(kind, rel_path, None, name)` — same
//!   canonical stream used by the full-reindex path, so overlay uids match
//!   base-graph uids for the same symbol.
//! - `file_idx`: 0 (placeholder; overlay nodes have no base-graph file entry
//!   until full merge promotion in T7-7).
//! - `content_hash`: 0 (fragment bins don't carry the source-byte hash yet).
//!
//! Fragment bins with `parse_failed = true` are skipped.

use ecp_core::graph::{Node, NodeKind};
use ecp_core::pool::{StrRef, StringPool};
use ecp_core::session::{DirtyFiles, Overlay};
use rkyv::rancor::Error as RkyvError;
use std::fs;
use std::io;
use std::path::Path;

use super::overlay_writer::ArchivedFragment;

/// Read all non-failed fragments from `session_dir` and materialise them as
/// an `Overlay`. Returns `None` when `dirty_files.json` is absent or empty.
pub fn load_overlay(session_dir: &Path) -> io::Result<Option<Overlay>> {
    let manifest = session_dir.join("dirty_files.json");
    if !manifest.exists() {
        return Ok(None);
    }
    let df = DirtyFiles::read(&manifest)?;
    if df.entries.is_empty() {
        return Ok(None);
    }

    let overlay_dir = session_dir.join("graph_overlay");
    let mut pool = StringPool::new();
    let mut nodes: Vec<Node> = Vec::new();

    for (rel_path, entry) in &df.entries {
        if entry.parse_failed {
            continue;
        }
        let bin_path = overlay_dir.join(format!("{}.bin", entry.fragment_id));
        let bytes = match fs::read(&bin_path) {
            Ok(b) => b,
            Err(_) => continue, // fragment file not written yet or already promoted
        };
        // Each bin is a rkyv-archived Vec<Fragment>.
        let archived =
            match rkyv::access::<rkyv::vec::ArchivedVec<ArchivedFragment>, RkyvError>(&bytes) {
                Ok(a) => a,
                Err(_) => continue, // corrupt fragment — skip
            };
        for frag in archived.iter() {
            let name_str = frag.name.as_str();
            let kind = NodeKind::from(&frag.kind);
            let uid = ecp_core::uid::compute(kind, rel_path, None, name_str);
            let name_ref: StrRef = pool.add(name_str);
            nodes.push(Node {
                uid,
                name: name_ref,
                file_idx: 0,
                kind,
                span: (
                    frag.span.0.to_native(),
                    frag.span.1.to_native(),
                    frag.span.2.to_native(),
                    frag.span.3.to_native(),
                ),
                community_id: 0,
                owner_class: StrRef::default(),
                content_hash: 0,
            });
        }
    }

    if nodes.is_empty() {
        return Ok(None);
    }
    Ok(Some(Overlay::new(nodes)))
}
