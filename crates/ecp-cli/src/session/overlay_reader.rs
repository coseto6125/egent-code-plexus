//! L1 overlay reader: materialises the on-disk fragment set
//! (`dirty_files.json` + `graph_overlay/<id>.bin` files) for the query side.
//!
//! Three consumers, three shapes:
//! - `load_overlay_hits` — flat per-symbol rows for `find`'s overlay-only
//!   name matching.
//! - `load_overlay` — `ecp_core::session::Overlay` for `merge_archived`
//!   (inspect's node-set merge).
//! - `load_view_inputs` — per-file `OverlayFileInput`s (symbols + imports +
//!   calls) for the query-time `OverlayView` edge merge.
//!
//! Fragment → Node conversion:
//! - `uid`:  `ecp_core::uid::compute(kind, rel_path, owner, name)` — same
//!   canonical stream used by the full-reindex path, so overlay uids match
//!   base-graph uids for the same symbol. v1 bins carry no owner (methods
//!   keep the pre-v2 empty-owner uid); v2 bins match methods exactly.
//! - `file_idx`: 0 (placeholder; overlay nodes have no base-graph file entry
//!   until full merge promotion in T7-7).
//! - `content_hash`: 0 (fragment bins don't carry the source-byte hash yet).
//!
//! Fragment bins with `parse_failed = true` are skipped. Bins are decoded
//! per the manifest's `DirtyEntry.format` marker — never guessed.

use ecp_core::graph::{Node, NodeKind};
use ecp_core::pool::{StrRef, StringPool};
use ecp_core::session::{DirtyFiles, Overlay, OverlayFileInput, OverlaySymbol, FRAGMENT_FORMAT_V2};
use rkyv::rancor::Error as RkyvError;
use std::fs;
use std::io;
use std::path::Path;

use super::overlay_writer::{ArchivedFragment, ArchivedFragmentFile};

/// A single overlay symbol with the source-relative path it came from.
///
/// `load_overlay` drops the rel_path (overlay `Node.file_idx` is a 0
/// placeholder until T7-7 promotion), which is fine for uid-keyed merge but
/// loses the file a `find` hit must display. This carries it through for the
/// query-visibility path so `ecp find <new_symbol>` can surface an
/// overlay-only symbol with its real file:line.
pub struct OverlayHit {
    pub uid: u64,
    pub name: String,
    pub kind: NodeKind,
    pub rel_path: String,
    /// 1-based start line (matches `Node::start_line` convention).
    pub line: u32,
}

/// Borrowed per-symbol view shared by both fragment encodings so the
/// `load_overlay*` consumers stay agnostic of the bin version. `uid` is
/// computed once at dispatch (owner-aware for v2 bins) so consumers can't
/// drift on the canonical stream.
struct FragSym<'a> {
    uid: u64,
    name: &'a str,
    kind: NodeKind,
    /// `(start_row, start_col, end_row, end_col)` — 0-based.
    span: (u32, u32, u32, u32),
    owner_class: Option<&'a str>,
}

impl<'a> FragSym<'a> {
    fn new(
        rel_path: &str,
        name: &'a str,
        kind: NodeKind,
        span: (u32, u32, u32, u32),
        owner_class: Option<&'a str>,
    ) -> Self {
        Self {
            uid: ecp_core::uid::compute(kind, rel_path, owner_class, name),
            name,
            kind,
            span,
            owner_class,
        }
    }
}

/// Visit every non-failed fragment symbol under `session_dir`, calling `f`
/// with the owning rel_path and a version-agnostic symbol view. Returns
/// `false` when there is no overlay (`dirty_files.json` absent or empty) so
/// callers can short-circuit; `true` when the manifest was present (even if
/// every fragment was skipped). Shared by `load_overlay` and
/// `load_overlay_hits` so manifest-walk + decode-dispatch live in one place.
fn for_each_fragment(session_dir: &Path, mut f: impl FnMut(&str, FragSym<'_>)) -> io::Result<bool> {
    let manifest = session_dir.join("dirty_files.json");
    if !manifest.exists() {
        return Ok(false);
    }
    let df = DirtyFiles::read(&manifest)?;
    if df.entries.is_empty() {
        return Ok(false);
    }
    let overlay_dir = session_dir.join("graph_overlay");
    for (rel_path, entry) in &df.entries {
        if entry.parse_failed {
            continue;
        }
        let bin_path = overlay_dir.join(format!("{}.bin", entry.fragment_id));
        let Ok(bytes) = fs::read(&bin_path) else {
            continue; // fragment not written yet or already promoted
        };
        if entry.format >= FRAGMENT_FORMAT_V2 {
            let Ok(archived) = rkyv::access::<ArchivedFragmentFile, RkyvError>(&bytes) else {
                continue; // corrupt fragment — skip
            };
            for sym in archived.symbols.iter() {
                f(
                    rel_path,
                    FragSym::new(
                        rel_path,
                        sym.name.as_str(),
                        NodeKind::from(&sym.kind),
                        (
                            sym.span.0.to_native(),
                            sym.span.1.to_native(),
                            sym.span.2.to_native(),
                            sym.span.3.to_native(),
                        ),
                        sym.owner_class.as_ref().map(|o| o.as_str()),
                    ),
                );
            }
        } else {
            let Ok(archived) =
                rkyv::access::<rkyv::vec::ArchivedVec<ArchivedFragment>, RkyvError>(&bytes)
            else {
                continue; // corrupt fragment — skip
            };
            for frag in archived.iter() {
                f(
                    rel_path,
                    FragSym::new(
                        rel_path,
                        frag.name.as_str(),
                        NodeKind::from(&frag.kind),
                        (
                            frag.span.0.to_native(),
                            frag.span.1.to_native(),
                            frag.span.2.to_native(),
                            frag.span.3.to_native(),
                        ),
                        None,
                    ),
                );
            }
        }
    }
    Ok(true)
}

/// Read all non-failed overlay fragments as `OverlayHit`s, preserving each
/// symbol's rel_path. Returns an empty Vec when there is no overlay — callers
/// on the query hot path pay nothing on a clean working tree.
pub fn load_overlay_hits(session_dir: &Path) -> io::Result<Vec<OverlayHit>> {
    let mut hits = Vec::new();
    for_each_fragment(session_dir, |rel_path, sym| {
        hits.push(OverlayHit {
            uid: sym.uid,
            name: sym.name.to_string(),
            kind: sym.kind,
            rel_path: rel_path.to_string(),
            // span.0 is the 0-based start row; +1 to match start_line().
            line: sym.span.0 + 1,
        });
    })?;
    Ok(hits)
}

/// Read all non-failed fragments from `session_dir` and materialise them as
/// an `Overlay`. Returns `None` when `dirty_files.json` is absent or empty.
pub fn load_overlay(session_dir: &Path) -> io::Result<Option<Overlay>> {
    let mut pool = StringPool::new();
    let mut nodes: Vec<Node> = Vec::new();
    for_each_fragment(session_dir, |_rel_path, sym| {
        let name_ref: StrRef = pool.add(sym.name);
        let owner_ref: StrRef = sym.owner_class.map(|o| pool.add(o)).unwrap_or_default();
        nodes.push(Node {
            uid: sym.uid,
            name: name_ref,
            file_idx: 0,
            kind: sym.kind,
            span: sym.span,
            community_id: 0,
            owner_class: owner_ref,
            content_hash: 0,
        });
    })?;

    if nodes.is_empty() {
        return Ok(None);
    }
    Ok(Some(Overlay::new(nodes)))
}

/// Read the dirty-file set as `OverlayView` inputs: one `OverlayFileInput`
/// per VALID v2 manifest entry. Entry validation guards the two staleness
/// holes the manifest has by construction:
///
/// - **format < v2** — written by an older binary; carries no calls/imports.
///   Treating the file as clean (skip) is the conservative choice: the next
///   `ensure_fresh` on a still-dirty file rewrites the bin as v2.
/// - **mtime mismatch** — `commit_fragment_results` only ever inserts
///   entries, so a file edited and then REVERTED keeps its (now stale)
///   manifest entry. Serving that fragment would beat fresh base data with
///   stale overlay data; the on-disk mtime is the cheap tell (the entry's
///   `mtime_ns` was stat'ed at fragment-write time from the same path).
///
/// Returns an empty Vec on a clean tree — `OverlayView::build` then returns
/// `None` and query paths stay on their original branch.
pub fn load_view_inputs(
    session_dir: &Path,
    worktree_root: &Path,
) -> io::Result<Vec<OverlayFileInput>> {
    let manifest = session_dir.join("dirty_files.json");
    if !manifest.exists() {
        return Ok(Vec::new());
    }
    let df = DirtyFiles::read(&manifest)?;
    let overlay_dir = session_dir.join("graph_overlay");
    let mut out = Vec::new();

    for (rel_path, entry) in &df.entries {
        if entry.parse_failed || entry.format < FRAGMENT_FORMAT_V2 {
            continue;
        }
        let on_disk_mtime_ns = crate::auto_ensure::mtime_ns_of(&worktree_root.join(rel_path));
        if on_disk_mtime_ns != Some(entry.mtime_ns) {
            continue; // reverted, deleted, or touched since the fragment write
        }
        let bin_path = overlay_dir.join(format!("{}.bin", entry.fragment_id));
        let Ok(bytes) = fs::read(&bin_path) else {
            continue;
        };
        let Ok(archived) = rkyv::access::<ArchivedFragmentFile, RkyvError>(&bytes) else {
            continue;
        };
        // Symbols copy straight off the archived view — a full
        // `rkyv::deserialize` of the whole FragmentFile would allocate every
        // string twice (once into FragmentFile, once into OverlaySymbol).
        // Imports keep the one-line deserialize: their `binding_kind` enum
        // makes a manual field map noisier than the small allocation it saves.
        let Ok(imports) = rkyv::deserialize::<Vec<ecp_core::analyzer::types::RawImport>, RkyvError>(
            &archived.imports,
        ) else {
            continue;
        };
        out.push(OverlayFileInput {
            rel_path: rel_path.clone(),
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
                    calls: s.calls.iter().map(|c| c.as_str().to_owned()).collect(),
                })
                .collect(),
            imports,
        });
    }
    Ok(out)
}
