//! L1 overlay reader: materialises the on-disk fragment set
//! (`l1/dirty_events.json` + `l1/fragments/<id>.bin` files) for the query
//! side. The legacy namespace an older binary writes is never read.
//!
//! `view_inputs` + `decode_view_inputs` build the per-file
//! `OverlayFileInput`s (symbols + imports + calls + export flags and
//! aliases) and the removed paths for the query-time `OverlayView` edge
//! merge; `decode_view_inputs_with_records` adds each file's
//! `OverlayFileRecords` for `OverlayView::build_with_records`. The first
//! pass validates the entries and hashes them into the view cache's input
//! key; only a cache miss pays for the second.
//!
//! Fragment bins with `parse_failed = true` are skipped. A tombstone (a
//! deleted file, the old half of a rename) is a removed path while the file
//! is still absent. Only the current fragment format is read; older entries
//! are re-parsed by the freshness gate.

use ecp_core::analyzer::types::{RawExportAlias, RawImport};
use ecp_core::contracts::routing::{ParsedLanguage, SizeCap};
use ecp_core::graph::NodeKind;
use ecp_core::session::layout;
use ecp_core::session::view::{ArchivedOverlayFileRecords, OverlayFileRecords};
use ecp_core::session::{DirtyEvents, OverlayFileInput, OverlaySymbol, FRAGMENT_FORMAT_V7};
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

/// Whether `bytes` has the current fragment archive shape and holds a
/// language the overlay can read: `ParsedLanguage::ABSENT` or a pair
/// `ParsedLanguage::from_bytes` accepts. A bin that names no language is
/// corrupt, never read as "no language", so the file re-parses.
///
/// The freshness gate uses this same check before it skips a re-parse. A
/// regular file alone is not enough: the view reader ignores malformed bins.
pub(crate) fn valid_fragment(bytes: &[u8]) -> bool {
    rkyv::access::<ArchivedFragmentFile, RkyvError>(bytes)
        .is_ok_and(|archived| fragment_language(archived).is_some())
}

/// The language `archived` holds: `Some(None)` for `ParsedLanguage::ABSENT`
/// (the parse decided none), `None` for bytes that name no language.
fn fragment_language(archived: &ArchivedFragmentFile) -> Option<Option<ParsedLanguage>> {
    let bytes = archived.parsed_language;
    match ParsedLanguage::from_bytes(bytes) {
        None if bytes != ParsedLanguage::ABSENT => None,
        parsed => Some(parsed),
    }
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
/// - **an entry of another fragment format** — a fragment id is a hash of the
///   route key and the content (V6); a V5 id is the content hash alone, so a
///   V5 bin can hold the parse of another route. The format check drops such
///   an entry and the freshness gate re-parses it, so the reader never serves
///   it.
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
        if entry.parse_failed || entry.format != FRAGMENT_FORMAT_V7 {
            continue;
        }
        if on_disk_mtime_ns() != Some(entry.mtime_ns) {
            continue; // reverted, deleted, or touched since the fragment write
        }
        let Ok(bytes) = fs::read(layout::fragment_path(session_dir, fragment_id)) else {
            continue;
        };
        if !valid_fragment(&bytes) {
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
        .filter_map(|fragment| decode_fragment(fragment).map(|(input, _)| input))
        .collect()
}

/// [`decode_view_inputs`] with each input's records, aligned with the
/// inputs: `OverlayView::build_with_records`' `files` and `records`. A
/// fragment whose records do not deserialize is dropped from both, by the
/// same deterministic rule.
pub fn decode_view_inputs_with_records(
    accepted: &[AcceptedFragment],
) -> (Vec<OverlayFileInput>, Vec<OverlayFileRecords>) {
    accepted
        .iter()
        .filter_map(|fragment| {
            let (input, records) = decode_fragment(fragment)?;
            let records = rkyv::deserialize::<OverlayFileRecords, RkyvError>(records).ok()?;
            Some((input, records))
        })
        .unzip()
}

/// One fragment's input, and its archived records.
fn decode_fragment(
    fragment: &AcceptedFragment,
) -> Option<(OverlayFileInput, &ArchivedOverlayFileRecords)> {
    let archived = rkyv::access::<ArchivedFragmentFile, RkyvError>(&fragment.bytes).ok()?;
    let parsed_language = fragment_language(archived)?;
    // Symbols copy straight off the archived view — a full
    // `rkyv::deserialize` of the whole FragmentFile would allocate every
    // string twice (once into FragmentFile, once into OverlaySymbol).
    // Imports and export aliases keep the one-line deserialize: the
    // `binding_kind` enum makes a manual field map noisier than the small
    // allocation it saves.
    let (Ok(imports), Ok(export_aliases)) = (
        rkyv::deserialize::<Vec<RawImport>, RkyvError>(&archived.imports),
        rkyv::deserialize::<Vec<RawExportAlias>, RkyvError>(&archived.export_aliases),
    ) else {
        return None;
    };
    let input = OverlayFileInput {
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
                generic_arity: s.generic_arity.to_native(),
                flags: s.flags.as_ref().map(|flags| flags.to_native()),
            })
            .collect(),
        imports,
        export_aliases,
        parsed_language,
    };
    Some((input, &archived.records))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::overlay_writer::{FragmentFile, FragmentSymbol};
    use rkyv::{Archive, Serialize};

    /// A `FragmentFile` as V7 bins held it before the records joined the
    /// payload.
    #[derive(Archive, Serialize)]
    struct FragmentFileWithoutRecords {
        symbols: Vec<FragmentSymbol>,
        imports: Vec<RawImport>,
        export_aliases: Vec<RawExportAlias>,
    }

    /// A `FragmentFile` as V7 bins held it before the file's language joined
    /// the payload.
    #[derive(Archive, Serialize)]
    struct FragmentFileWithoutLanguage {
        symbols: Vec<FragmentSymbol>,
        imports: Vec<RawImport>,
        export_aliases: Vec<RawExportAlias>,
        records: OverlayFileRecords,
    }

    fn accepted(bytes: Vec<u8>) -> AcceptedFragment {
        AcceptedFragment {
            rel_path: "src/a.ts".into(),
            bytes,
        }
    }

    /// Stands for: a bin of the payload before the records is unreadable,
    /// so the freshness gate re-parses it and the reader serves nothing of
    /// it, never a half-decoded file with no records.
    #[test]
    fn test_valid_fragment_bin_without_records_rejected() {
        let old = FragmentFileWithoutRecords {
            symbols: Vec::new(),
            imports: Vec::new(),
            export_aliases: Vec::new(),
        };
        let bytes = rkyv::to_bytes::<RkyvError>(&old).unwrap().into_vec();
        assert!(!valid_fragment(&bytes));
        let current = rkyv::to_bytes::<RkyvError>(&FragmentFile::default())
            .unwrap()
            .into_vec();
        assert!(valid_fragment(&current));
    }

    /// Stands for: both decoders serve the same inputs, and the records one
    /// aligns one record set with each input.
    #[test]
    fn test_decode_view_inputs_with_records_aligns_records_with_inputs() {
        let mut fragment = FragmentFile::default();
        fragment
            .records
            .tx_scopes
            .push(ecp_core::analyzer::types::RawTxScope::new(
                0,
                ecp_core::analyzer::types::FrameworkId::SpringTransactional,
            ));
        let bytes = rkyv::to_bytes::<RkyvError>(&fragment).unwrap().into_vec();
        let fragments = [accepted(bytes.clone()), accepted(bytes)];
        let (inputs, records) = decode_view_inputs_with_records(&fragments);
        assert_eq!(inputs.len(), 2);
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|r| r.tx_scopes.len() == 1));
        let plain = decode_view_inputs(&fragments);
        assert_eq!(
            plain.iter().map(|i| &i.rel_path).collect::<Vec<_>>(),
            inputs.iter().map(|i| &i.rel_path).collect::<Vec<_>>()
        );
    }

    /// Stands for: a bin written before the language joined the payload is
    /// unreadable, so the freshness gate re-parses it and the reader serves
    /// nothing of it. Never a decoded file whose language defaults to the
    /// path's. The empty payload is shorter than the current root, which
    /// the archive check rejects by length.
    #[test]
    fn test_valid_fragment_bin_without_parsed_language_rejected() {
        let old = FragmentFileWithoutLanguage {
            symbols: Vec::new(),
            imports: Vec::new(),
            export_aliases: Vec::new(),
            records: OverlayFileRecords::default(),
        };
        let bytes = rkyv::to_bytes::<RkyvError>(&old).unwrap().into_vec();
        assert!(!valid_fragment(&bytes));
        assert!(decode_view_inputs(&[accepted(bytes)]).is_empty());
    }

    /// Stands for: bytes that name no language are corrupt, never "no
    /// language". `ABSENT` is the only spelling of "the parse decided none".
    #[test]
    fn test_valid_fragment_unnamed_language_bytes_rejected() {
        let with = |parsed_language: [u8; 2]| {
            let fragment = FragmentFile {
                parsed_language,
                ..FragmentFile::default()
            };
            rkyv::to_bytes::<RkyvError>(&fragment).unwrap().into_vec()
        };
        assert!(valid_fragment(&with(ParsedLanguage::ABSENT)));
        assert!(!valid_fragment(&with([0xFE, 0xFE])), "no such language");
        assert!(
            !valid_fragment(&with([ParsedLanguage::ABSENT[0], 1])),
            "a script byte beside an absent file byte is a provider's half state, never stored"
        );
        let decoded = decode_view_inputs(&[accepted(with([0xFE, 0xFE]))]);
        assert!(decoded.is_empty(), "a corrupt bin decodes to nothing");
    }

    /// Stands for: `FragmentFile::default()` is "no language", not the
    /// all-zero pair, which names a real language.
    #[test]
    fn test_fragment_file_default_parsed_language_is_absent() {
        assert_eq!(
            FragmentFile::default().parsed_language,
            ParsedLanguage::ABSENT
        );
        let decoded = decode_view_inputs(&[accepted(
            rkyv::to_bytes::<RkyvError>(&FragmentFile::default())
                .unwrap()
                .into_vec(),
        )]);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].parsed_language, None);
    }
}
