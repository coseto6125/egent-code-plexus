//! L1 overlay reader: materialises the on-disk fragment set
//! (`l1/dirty_events.json` + `l1/fragments/<id>.bin` files) for the query
//! side. The legacy namespace an older binary writes is never read.
//!
//! `view_inputs` + `decode_view_inputs` build the per-file
//! `OverlayFileInput`s (symbols + imports + calls + export flags and
//! aliases) and the removed paths for the query-time `OverlayView` edge
//! merge. The first pass validates the entries and hashes them into the
//! view cache's input key; only a cache miss pays for the second.
//!
//! Fragment bins with `parse_failed = true` are skipped. A tombstone (a
//! deleted file, the old half of a rename) is a removed path while the file
//! is still absent. Only the current fragment format is read; older entries
//! are re-parsed by the freshness gate.

use ecp_core::analyzer::types::{RawExportAlias, RawImport};
use ecp_core::contracts::routing::SizeCap;
use ecp_core::graph::NodeKind;
use ecp_core::session::layout;
use ecp_core::session::{DirtyEvents, OverlayFileInput, OverlaySymbol, FRAGMENT_FORMAT_V5};
use rkyv::rancor::Error as RkyvError;
use std::fs;
use std::io;
use std::path::Path;
use xxhash_rust::xxh3::Xxh3;

use super::overlay_writer::ArchivedFragmentFile;

/// A manifest entry `view_inputs` let through: its rel_path and the
/// fragment bytes that passed archive validation.
pub struct AcceptedFragment {
    rel_path: String,
    bytes: Vec<u8>,
}

/// Whether `bytes` has the current fragment archive shape.
///
/// The freshness gate uses this same check before it skips a re-parse. A
/// regular file alone is not enough: the view reader ignores malformed bins.
pub(crate) fn valid_v5_fragment(bytes: &[u8]) -> bool {
    rkyv::access::<ArchivedFragmentFile, RkyvError>(bytes).is_ok()
}

/// One manifest read's share of `OverlayView`: the fragments the
/// acceptance rules let through, the tombstoned paths still absent from the
/// worktree, and `hash`, an xxh3 over both in manifest order (the inputs'
/// share of the view cache key).
pub struct ViewInputs {
    pub fragments: Vec<AcceptedFragment>,
    /// Repo-relative paths of deleted files and of the old halves of
    /// renames: `OverlayView::build_with_removed`'s `removed`.
    pub tombstones: Vec<String>,
    pub hash: u64,
}

impl ViewInputs {
    /// True on a clean tree: query paths then stay on their original branch.
    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty() && self.tombstones.is_empty()
    }
}

/// [`view_inputs`]'s fragments and hash. The hash covers the tombstones too.
pub fn accepted_fragments(
    session_dir: &Path,
    worktree_root: &Path,
) -> io::Result<(Vec<AcceptedFragment>, u64)> {
    view_inputs(session_dir, worktree_root).map(|inputs| (inputs.fragments, inputs.hash))
}

/// The dirty-file entries that may feed `OverlayView`, plus an xxh3 over
/// each one's rel_path and fragment bytes (an empty part for a tombstone) in
/// manifest order. Entry validation guards the staleness holes the manifest
/// has by construction:
///
/// - **another format, builder or cap** — a manifest that fails
///   `DirtyEvents::is_current` under the worktree's size cap holds parse
///   output of other rules (a v2 bin has no export aliases at all; a file
///   over another cap parsed to an omission). Treating its files as clean
///   (skip) is the conservative choice: the next `ensure_fresh` re-parses
///   every dirty file into a fresh manifest. Its tombstones are skipped too.
/// - **mtime mismatch** — a file edited and then REVERTED keeps its (now
///   stale) manifest entry until the next `ensure_fresh` retires it
///   (`retire_unclaimed_entries`). Serving that fragment would beat fresh
///   base data with stale overlay data; the on-disk mtime is the cheap tell
///   (the entry's `mtime_ns` was stat'ed at fragment-write time from the
///   same path).
/// - **a tombstoned file is back** — the tombstone stays in the manifest
///   when its file comes back, so it counts only while the path is absent,
///   by the freshness gate's own stat rule (`mtime_ns_of` gives `None`).
///
/// Returns empty lists on a clean tree. The same pass computes the lists
/// and the hash, so the key covers exactly the entries the acceptance rules
/// let through.
pub fn view_inputs(session_dir: &Path, worktree_root: &Path) -> io::Result<ViewInputs> {
    view_inputs_under(
        session_dir,
        worktree_root,
        ecp_core::config::resolve_size_cap(worktree_root),
    )
}

/// `view_inputs` under a cap the caller resolved once for the query.
pub fn view_inputs_under(
    session_dir: &Path,
    worktree_root: &Path,
    size_cap: SizeCap,
) -> io::Result<ViewInputs> {
    let mut hasher = Xxh3::new();
    let mut fragments = Vec::new();
    let mut tombstones = Vec::new();
    let manifest = layout::dirty_events_path(session_dir);
    let manifest = if manifest.exists() {
        Some(DirtyEvents::read(&manifest)?)
    } else {
        None
    };
    let entries = manifest
        .iter()
        .filter(|events| events.is_current(size_cap))
        .flat_map(|events| &events.entries);

    for (rel_path, entry) in entries {
        let on_disk_mtime_ns = || crate::auto_ensure::mtime_ns_of(&worktree_root.join(rel_path));
        if entry.is_tombstone() {
            if on_disk_mtime_ns().is_none() {
                // An accepted fragment's bytes pass archive validation and
                // are never empty, so the empty part marks a tombstone.
                super::view_cache::hash_part(&mut hasher, rel_path.as_bytes());
                super::view_cache::hash_part(&mut hasher, &[]);
                tombstones.push(rel_path.clone());
            }
            continue;
        }
        let Some(fragment_id) = entry.fragment_id.as_deref() else {
            continue; // no bin, and no deletion either
        };
        if entry.parse_failed || entry.format != FRAGMENT_FORMAT_V5 {
            continue;
        }
        if on_disk_mtime_ns() != Some(entry.mtime_ns) {
            continue; // reverted, deleted, or touched since the fragment write
        }
        let Ok(bytes) = fs::read(layout::fragment_path(session_dir, fragment_id)) else {
            continue;
        };
        if !valid_v5_fragment(&bytes) {
            continue;
        }
        super::view_cache::hash_part(&mut hasher, rel_path.as_bytes());
        super::view_cache::hash_part(&mut hasher, &bytes);
        fragments.push(AcceptedFragment {
            rel_path: rel_path.clone(),
            bytes,
        });
    }
    Ok(ViewInputs {
        fragments,
        tombstones,
        hash: hasher.digest(),
    })
}

/// The `OverlayView` inputs of `accepted`, one per fragment whose imports
/// and export aliases deserialize. Dropping the rest keeps the cache key
/// sound: this is a deterministic function of the bytes the key hashed, so
/// equal keys still mean equal inputs.
pub fn decode_view_inputs(accepted: &[AcceptedFragment]) -> Vec<OverlayFileInput> {
    accepted
        .iter()
        .filter_map(|fragment| {
            let archived = rkyv::access::<ArchivedFragmentFile, RkyvError>(&fragment.bytes).ok()?;
            // Symbols copy straight off the archived view — a full
            // `rkyv::deserialize` of the whole FragmentFile would allocate
            // every string twice (once into FragmentFile, once into
            // OverlaySymbol). Imports and export aliases keep the one-line
            // deserialize: the `binding_kind` enum makes a manual field map
            // noisier than the small allocation it saves.
            let (Ok(imports), Ok(export_aliases)) = (
                rkyv::deserialize::<Vec<RawImport>, RkyvError>(&archived.imports),
                rkyv::deserialize::<Vec<RawExportAlias>, RkyvError>(&archived.export_aliases),
            ) else {
                return None;
            };
            Some(OverlayFileInput {
                rel_path: fragment.rel_path.clone(),
                symbols: archived
                    .symbols
                    .iter()
                    .map(|s| OverlaySymbol {
                        name: s.name.as_str().to_owned(),
                        kind: NodeKind::from(&s.kind),
                        owner_class: s.owner_class.as_ref().map(|o| o.as_str().to_owned()),
                        // span rows are 0-based; Node line conventions are 1-based.
                        start_line: s.span.0.to_native() + 1,
                        end_line: s.span.2.to_native() + 1,
                        start_column: s.span.1.to_native(),
                        end_column: s.span.3.to_native(),
                        calls: s.calls.iter().map(|c| c.as_str().to_owned()).collect(),
                        is_exported: s.is_exported,
                        heritage: s.heritage.iter().map(|h| h.as_str().to_owned()).collect(),
                        variant_refs: s
                            .variant_refs
                            .iter()
                            .map(|v| v.as_str().to_owned())
                            .collect(),
                        return_type: s.return_type.as_ref().map(|ty| ty.as_str().to_owned()),
                    })
                    .collect(),
                imports,
                export_aliases,
            })
        })
        .collect()
}
