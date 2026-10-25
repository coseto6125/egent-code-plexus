//! L1 fragment writer: re-parses a dirty source file, archives the per-file
//! graph fragment via rkyv into `l1/fragments/`, updates `l1/dirty_events.json`
//! (tombstones included) + `session_meta.overlay_version`.
//!
//! Atomic semantics: each fragment is written to `<id>.tmp` → fsync → rename,
//! so a reader merging fragments can never see partial state. The manifest
//! rewrite happens after the fragment rename, in the same `atomic_write_json`
//! style — readers always see a consistent snapshot.
//!
//! Several items below (`OverlayWriter` struct + impl, `pipeline_for_path` /
//! `map_node_kind` helpers, `relativise`, `extract_symbols`) are the
//! write-time symbol-extraction surface spec'd by
//! `docs/superpowers/specs/2026-05-17-multi-agent-peer-sync-design.md:152`
//! and `docs/superpowers/plans/2026-05-17-multi-agent-peer-sync.md` Task 2.
//! They are intentionally pre-wired ahead of the watcher integration that
//! will drive them — `#![allow(dead_code)]` is retained here for the
//! watcher-integration items; the merge-consumer items (`Fragment`,
//! `ArchivedFragment`) are now used by `overlay_reader.rs`.

#![allow(dead_code)]

use ecp_core::analyzer::types::{
    LocalGraph, RawEventTopic, RawExportAlias, RawImport, RawSchemaField, RawTxScope,
};
use ecp_core::contracts::dirty_event::{PathChange, PathClass};
use ecp_core::contracts::routing::{FileRouter, RouteOutcome, SizeCap};
use ecp_core::graph::NodeKind;
use ecp_core::registry::{atomic_write_json, replace_file, FileLock};
use ecp_core::session::layout;
use ecp_core::session::overlay::{SymbolKind, SymbolRef};
use ecp_core::session::{DirtyEventEntry, DirtyEvents, SessionMeta};
use rayon::prelude::*;
use rkyv::{Archive, Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use xxhash_rust::xxh3::xxh3_64;

/// A single symbol span produced by `parse_to_fragment`.
///
/// Mirrors the shape emitted by full-reindex: `span` is `(start_row, start_col,
/// end_row, end_col)` — identical to `RawNode.span` so the T7-7 parity gate
/// can do a direct comparison between incremental and full-index paths.
///
/// `schema_fields`, `event_topics`, `tx_scopes` carry the T0-2 side-data vectors
/// so the struct shape aligns with full-reindex output from day one; the Phase 3
/// detectors fill them — they are empty here.
#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct Fragment {
    pub name: String,
    pub kind: NodeKind,
    /// `(start_row, start_col, end_row, end_col)` — 0-based, matches `RawNode.span`.
    pub span: (u32, u32, u32, u32),
    pub schema_fields: Vec<RawSchemaField>,
    pub event_topics: Vec<RawEventTopic>,
    pub tx_scopes: Vec<RawTxScope>,
}

/// Convert the nodes in a `LocalGraph` into `Fragment` slices.
///
/// Exposed for tests: callers that need to inspect fragment shape without
/// going through rkyv serialisation/deserialisation call this directly.
pub fn fragments_from_local_graph(graph: &LocalGraph) -> Vec<Fragment> {
    graph
        .nodes
        .iter()
        .map(|n| Fragment {
            name: n.name.clone(),
            kind: n.kind,
            span: n.span,
            schema_fields: vec![],
            event_topics: vec![],
            tx_scopes: vec![],
        })
        .collect()
}

/// One symbol in a v5 fragment bin. Compared to v1 `Fragment` it adds the
/// fields the query-time `OverlayView` needs: `owner_class` (so the
/// recomputed uid matches base-graph method uids exactly), `calls` (callee
/// short names — the raw material for overlay `Calls` edges),
/// `is_exported`, `heritage` (declared base names, as the parser wrote them)
/// and `variant_refs` (every raw `field_reads` entry, including tagged name
/// references); plus the paired function return type;
/// it drops the always-empty v1 detector vectors.
#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct FragmentSymbol {
    pub name: String,
    pub kind: NodeKind,
    /// `(start_row, start_col, end_row, end_col)` — 0-based.
    pub span: (u32, u32, u32, u32),
    pub owner_class: Option<String>,
    pub is_exported: bool,
    pub calls: Vec<String>,
    pub heritage: Vec<String>,
    pub variant_refs: Vec<String>,
    pub return_type: Option<String>,
    /// `RawNode::generic_arity`: a C# type's or delegate's declared generic
    /// arity, 0 for every other node. The overlay numbers a dirty `Conv<T>` beside `Conv`
    /// from it, as the full build does.
    pub generic_arity: u32,
    /// `RawFunctionMeta::flags` of a function, method or constructor whose
    /// span a meta carries; `None` otherwise. `Some(0)` is a real value
    /// (public instance), so absence is not zero.
    pub flags: Option<u16>,
}

/// The symbol shape of the v2 and v3 fragment bins (`FragmentSymbol` before
/// `heritage` and `variant_refs`), kept to read a bin an older binary wrote.
#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct FragmentSymbolV3 {
    pub name: String,
    pub kind: NodeKind,
    /// `(start_row, start_col, end_row, end_col)` — 0-based.
    pub span: (u32, u32, u32, u32),
    pub owner_class: Option<String>,
    pub is_exported: bool,
    pub calls: Vec<String>,
}

/// v5 fragment bin payload: the whole dirty file's fresh parse, including
/// file-level imports and export aliases for query-time Tier-2
/// (import-scoped) resolution. `DirtyEventEntry.format == FRAGMENT_FORMAT_V7`
/// marks bins of this shape — readers MUST dispatch on the manifest marker,
/// not guess the encoding.
#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct FragmentFile {
    pub symbols: Vec<FragmentSymbol>,
    pub imports: Vec<RawImport>,
    pub export_aliases: Vec<RawExportAlias>,
}

/// Exact v4 archive shape, retained to decode fragments written before v5.
#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct FragmentSymbolV4 {
    pub name: String,
    pub kind: NodeKind,
    pub span: (u32, u32, u32, u32),
    pub owner_class: Option<String>,
    pub is_exported: bool,
    pub calls: Vec<String>,
    pub heritage: Vec<String>,
    pub variant_refs: Vec<String>,
}

/// Exact v4 archive payload, retained for overlay hit readers.
#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct FragmentFileV4 {
    pub symbols: Vec<FragmentSymbolV4>,
    pub imports: Vec<RawImport>,
    pub export_aliases: Vec<RawExportAlias>,
}

/// The v3 fragment bin payload (`FragmentFile` before `heritage` and
/// `variant_refs`), kept to read the symbol list of a bin an older binary
/// wrote.
#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct FragmentFileV3 {
    pub symbols: Vec<FragmentSymbolV3>,
    pub imports: Vec<RawImport>,
    pub export_aliases: Vec<RawExportAlias>,
}

/// The v2 fragment bin payload (`FragmentFileV3` before `export_aliases`),
/// kept to read the symbol list of a bin an older binary wrote.
#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct FragmentFileV2 {
    pub symbols: Vec<FragmentSymbolV3>,
    pub imports: Vec<RawImport>,
}

pub fn fragment_file_from_local_graph(graph: &LocalGraph) -> FragmentFile {
    let metas: std::collections::HashMap<_, _> = graph
        .raw_function_metas
        .iter()
        .map(|meta| (meta.span, meta))
        .collect();
    FragmentFile {
        symbols: graph
            .nodes
            .iter()
            .map(|n| {
                let meta = matches!(
                    n.kind,
                    NodeKind::Function | NodeKind::Method | NodeKind::Constructor
                )
                .then(|| metas.get(&n.span))
                .flatten();
                FragmentSymbol {
                    name: n.name.clone(),
                    kind: n.kind,
                    span: n.span,
                    owner_class: n.owner_class.clone(),
                    is_exported: n.is_exported,
                    calls: n.calls.clone(),
                    heritage: n.heritage.clone(),
                    variant_refs: n.field_reads.clone(),
                    return_type: meta
                        .map(|meta| meta.return_type.clone())
                        .filter(|return_type| !return_type.is_empty()),
                    generic_arity: n.generic_arity(),
                    flags: meta.map(|meta| meta.flags),
                }
            })
            .collect(),
        imports: graph.imports.clone(),
        export_aliases: graph.export_aliases.as_deref().unwrap_or(&[]).to_vec(),
    }
}

pub struct FragmentInput {
    pub rel_path: String,
    pub content: Vec<u8>,
    pub mtime_ns: u64,
}

pub struct FragmentOutcome {
    // Asserted by `tests/overlay_writer.rs` + `tests/promotion.rs`; bin path
    // currently only branches on `parse_failed`.
    #[allow(dead_code)]
    pub fragment_id: String,
    pub parse_failed: bool,
}

/// The C3 change and class the freshness gate saw for one dirty path, as
/// the manifest stores them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyPathEvent {
    pub change: PathChange,
    pub class: PathClass,
}

impl DirtyPathEvent {
    /// What a writer records for a parsed file it has no event for (the
    /// `FragmentInput` route, tests): the file exists and is source.
    pub const PARSED_SOURCE: Self = Self {
        change: PathChange::Modified,
        class: PathClass::SOURCE,
    };

    /// The event of a file that exists and was parsed. The manifest stores
    /// confirmed changes only: an `unmerged` path that exists is `modified`
    /// (C3 `DirtyEvent::confirm`), and so is a `deleted` path that came back
    /// before the stat.
    fn parsed(self) -> Self {
        let change = match self.change {
            PathChange::Unmerged | PathChange::Deleted => PathChange::Modified,
            change => change,
        };
        Self { change, ..self }
    }
}

/// The cap of the worktree this session overlays. An unreadable meta fails
/// the commit later; until then the env or built-in cap stands.
fn session_size_cap(session_dir: &Path) -> SizeCap {
    SessionMeta::read(&session_dir.join("session_meta.json"))
        .map(|sm| ecp_core::config::resolve_size_cap(Path::new(&sm.source_worktree)))
        .unwrap_or_else(|_| ecp_core::config::env_size_cap(None))
}

/// Write or update a dirty-file fragment for the given session.
///
/// Pre-conditions: `session_dir` exists and contains a valid
/// `session_meta.json`. Caller is responsible for creating the session
/// dir + initial session meta — see auto_ensure / promotion paths.
pub fn write_dirty_fragment(
    session_dir: &Path,
    input: &FragmentInput,
) -> io::Result<FragmentOutcome> {
    let mut outcomes = write_dirty_fragments_batch(session_dir, std::slice::from_ref(input))?;
    Ok(outcomes.pop().expect("batch of 1 should return 1 outcome"))
}

/// Batched variant of `write_dirty_fragment`: writes N per-file fragments
/// and coalesces the manifest + overlay-version updates into one each.
///
/// Per-file fragment writes still go through atomic tmp → fsync → rename
/// against unique `<content_hash>.bin` paths (no inter-input collision); the
/// renames run under the session's overlay lock (see [`overlay_lock_path`]).
/// The single trailing manifest + version write replaces N read-modify-write
/// cycles in the L1 refresh path — the dominant cost on small dirty sets
/// (each `atomic_write_json` is a write + fsync + rename).
///
/// The fragment tmp path is keyed by `(batch_seq, input_idx)` so identical
/// content within the same batch (same `fragment_id`) cannot race on a
/// shared `<fragment_id>.tmp` under the rayon `par_iter` fan-out below.
///
/// `overlay_version` is bumped by `inputs.len()` to preserve the
/// "increment per write" semantics that single-call sites + existing tests
/// rely on.
pub fn write_dirty_fragments_batch(
    session_dir: &Path,
    inputs: &[FragmentInput],
) -> io::Result<Vec<FragmentOutcome>> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }

    let overlay_dir = layout::fragments_dir(session_dir);
    fs::create_dir_all(&overlay_dir)?;
    let batch_seq = WRITE_BATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    // Resolved once per batch: the parse and the manifest header share it.
    let size_cap = session_size_cap(session_dir);

    // Parallelise fragment writes: hashing and parsing (CPU) + atomic file write (IO).
    // Coalesces the per-file IO while keeping the final manifest commit atomic.
    let results: Vec<io::Result<StagedFragment>> = inputs
        .par_iter()
        .enumerate()
        .map(|(idx, input)| {
            write_fragment_file(&overlay_dir, batch_seq, idx as u64, input, size_cap)
        })
        .collect();

    commit_fragment_results(session_dir, size_cap, results, &[])
}

/// Write fragments from already-parsed `LocalGraph`s — the incremental path's
/// root optimisation: `reanalyze_files` parsed these files once already, so
/// reparsing them here (the old `FragmentInput` route) would pay the tree-sitter
/// cost twice. Each graph carries its own `content_hash` (same `xxh3_64` the
/// re-parse route hashes), so `fragment_id` stays byte-identical; `mtimes` is
/// the paired modification time per graph (stat is cheap, the graph doesn't
/// carry it). Graphs and `mtimes` MUST be equal length and aligned by index.
///
/// Records every graph as [`DirtyPathEvent::PARSED_SOURCE`] under the cap of
/// the session's worktree; [`write_dirty_events`] is the form that carries
/// the gate's events and tombstones.
pub fn write_dirty_fragments_from_graphs(
    session_dir: &Path,
    graphs: &[LocalGraph],
    mtimes: &[u64],
) -> io::Result<Vec<FragmentOutcome>> {
    write_dirty_events(
        session_dir,
        session_size_cap(session_dir),
        graphs,
        mtimes,
        &HashMap::new(),
        &[],
    )
}

/// The incremental path's writer: one fragment per parsed graph, recorded
/// with the event `events` holds for its path (else
/// [`DirtyPathEvent::PARSED_SOURCE`]), and one tombstone per
/// `(path, class)` in `tombstones` (a dirty source path absent at the
/// pre-parse stat). `size_cap` is the cap the graphs were parsed under; the
/// manifest records it, and a later cap re-parses every entry.
///
/// Returns one outcome per graph; tombstones have none.
pub fn write_dirty_events(
    session_dir: &Path,
    size_cap: SizeCap,
    graphs: &[LocalGraph],
    mtimes: &[u64],
    events: &HashMap<String, DirtyPathEvent>,
    tombstones: &[(String, PathClass)],
) -> io::Result<Vec<FragmentOutcome>> {
    if graphs.len() != mtimes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "graphs ({}) and mtimes ({}) must be aligned 1:1",
                graphs.len(),
                mtimes.len()
            ),
        ));
    }
    if graphs.is_empty() && tombstones.is_empty() {
        return Ok(Vec::new());
    }

    let overlay_dir = layout::fragments_dir(session_dir);
    fs::create_dir_all(&overlay_dir)?;
    let batch_seq = WRITE_BATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let worktree = SessionMeta::read(&session_dir.join("session_meta.json"))
        .map(|sm| PathBuf::from(sm.source_worktree))
        .unwrap_or_default();

    let results: Vec<io::Result<StagedFragment>> = graphs
        .par_iter()
        .zip(mtimes.par_iter())
        .enumerate()
        .map(|(idx, (graph, &mtime_ns))| {
            write_fragment_from_graph(
                &overlay_dir,
                batch_seq,
                idx as u64,
                graph,
                mtime_ns,
                events,
                &worktree,
            )
        })
        .collect();

    commit_fragment_results(session_dir, size_cap, results, tombstones)
}

/// The lock a writer holds while it publishes fragment bins and commits the
/// manifest that names them, and `admin gc` holds while it judges a bin
/// orphan and deletes it. A bin's name derives from its route key and content
/// hash alone, so without the lock gc could delete a bin a writer had just
/// re-published under the name gc judged orphan.
pub fn overlay_lock_path(session_dir: &Path) -> PathBuf {
    session_dir.join("overlay.lock")
}

/// One dirty file's fragment before publish: its manifest entry and, when
/// the archive was written, the fsynced tmp that becomes `<fragment_id>.bin`.
struct StagedFragment {
    rel_path: String,
    entry: DirtyEventEntry,
    outcome: FragmentOutcome,
    tmp: Option<PathBuf>,
}

/// Write `bytes` to a tmp beside the bin it will replace and fsync it. The
/// tmp name is keyed by `(batch_seq, input_idx)` so two inputs with the same
/// content never share one.
fn stage_archive(
    overlay_dir: &Path,
    fragment_id: &str,
    batch_seq: u64,
    input_idx: u64,
    bytes: &[u8],
) -> io::Result<PathBuf> {
    let tmp = overlay_dir.join(format!("{fragment_id}.{batch_seq}.{input_idx}.tmp"));
    fs::write(&tmp, bytes)?;
    fs::OpenOptions::new().write(true).open(&tmp)?.sync_all()?;
    Ok(tmp)
}

/// Shared tail for both write paths: collect per-file results, publish their
/// bins, then commit the manifest (fragments and tombstones) + the
/// `overlay_version` bump in one atomic write each (coalesces N
/// read-modify-write cycles down to 2).
fn commit_fragment_results(
    session_dir: &Path,
    size_cap: SizeCap,
    results: Vec<io::Result<StagedFragment>>,
    tombstones: &[(String, PathClass)],
) -> io::Result<Vec<FragmentOutcome>> {
    let staged = results.into_iter().collect::<io::Result<Vec<_>>>()?;
    let mut outcomes = Vec::with_capacity(staged.len());
    {
        let _publish = FileLock::acquire_exclusive(&overlay_lock_path(session_dir))?;
        for fragment in &staged {
            if let (Some(tmp), Some(id)) = (&fragment.tmp, &fragment.entry.fragment_id) {
                replace_file(tmp, &layout::fragment_path(session_dir, id))?;
            }
        }
        let manifest_path = layout::dirty_events_path(session_dir);
        // A manifest of another format, builder or cap, or one that does not
        // parse, holds entries the freshness gate re-parses whole: none is
        // kept. Replacing it (instead of failing the write) is what lets
        // that re-parse land.
        let mut manifest = DirtyEvents::read(&manifest_path)
            .ok()
            .filter(|m| m.is_current(size_cap))
            .unwrap_or_else(|| DirtyEvents::empty(size_cap));
        for fragment in staged {
            manifest.entries.insert(fragment.rel_path, fragment.entry);
            outcomes.push(fragment.outcome);
        }
        for (rel_path, class) in tombstones {
            manifest
                .entries
                .insert(rel_path.clone(), DirtyEventEntry::tombstone(*class));
        }
        DirtyEvents::write_atomic(&manifest_path, &manifest)?;
    }

    let sm_path = session_dir.join("session_meta.json");
    let mut sm = SessionMeta::read(&sm_path)?;
    sm.overlay_version += (outcomes.len() + tombstones.len()) as u32;
    sm.last_touched = chrono::Utc::now().to_rfc3339();
    atomic_write_json(&sm_path, &sm)?;

    Ok(outcomes)
}

/// Drops each manifest entry `stale` names, under the publish lock, when the
/// entry still equals the one the caller saw. An entry published or rewritten
/// since the caller's snapshot differs from it, so it stays. A manifest left with no entry is removed, so a clean tree costs the reader
/// one failed open. The bins stay for `admin gc`: a `fragment_id` is a
/// content hash, so another entry can share the bin.
pub fn retire_entries(
    session_dir: &Path,
    stale: &HashMap<String, DirtyEventEntry>,
) -> io::Result<usize> {
    let _publish = FileLock::acquire_exclusive(&overlay_lock_path(session_dir))?;
    let manifest_path = layout::dirty_events_path(session_dir);
    // A missing or unparsable manifest has no entry a reader serves.
    let Ok(mut manifest) = DirtyEvents::read(&manifest_path) else {
        return Ok(0);
    };
    let before = manifest.entries.len();
    manifest
        .entries
        .retain(|path, entry| stale.get(path) != Some(entry));
    let retired = before - manifest.entries.len();
    if retired == 0 {
        return Ok(0);
    }
    if manifest.entries.is_empty() {
        match fs::remove_file(&manifest_path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    } else {
        DirtyEvents::write_atomic(&manifest_path, &manifest)?;
    }
    Ok(retired)
}

fn write_fragment_file(
    overlay_dir: &Path,
    batch_seq: u64,
    input_idx: u64,
    input: &FragmentInput,
    size_cap: SizeCap,
) -> io::Result<StagedFragment> {
    let content_hash = xxh3_64(&input.content);
    // Same forward-slash invariant as `write_fragment_from_graph`.
    let rel_path = input.rel_path.replace('\\', "/");
    let fragment_id = fragment_id_for(&rel_path, content_hash);
    let content_hash = format!("{content_hash:016x}");
    // Parse and archive stay one fallible unit, as they were when this called a
    // combined `parse_to_fragment`: an rkyv failure marks the entry
    // `parse_failed` like a parse failure does, rather than aborting the whole
    // batch write.
    let parsed = parse_graph(&input.rel_path, &input.content, size_cap)
        .and_then(|graph| archive_fragments(&graph).map(|bytes| (graph, bytes)));
    let (tmp, dirty_symbols) = match parsed {
        Ok((graph, archive_bytes)) => (
            Some(stage_archive(
                overlay_dir,
                &fragment_id,
                batch_seq,
                input_idx,
                &archive_bytes,
            )?),
            symbols_from_graph(&graph, &rel_path),
        ),
        Err(_) => (None, Vec::new()),
    };
    let parse_failed = tmp.is_none();

    let event = DirtyPathEvent::PARSED_SOURCE;
    let entry = DirtyEventEntry {
        change: event.change,
        class: event.class,
        mtime_ns: input.mtime_ns,
        fragment_id: Some(fragment_id.clone()),
        content_hash: Some(content_hash),
        parse_failed,
        dirty_symbols,
        format: ecp_core::session::overlay::FRAGMENT_FORMAT_V7,
    };

    let outcome = FragmentOutcome {
        fragment_id,
        parse_failed,
    };

    Ok(StagedFragment {
        rel_path,
        entry,
        outcome,
        tmp,
    })
}

fn parse_graph(rel_path: &str, content: &[u8], size_cap: SizeCap) -> io::Result<LocalGraph> {
    let path = std::path::Path::new(rel_path);
    pipeline_for_path(path, size_cap)
        .ok_or_else(|| io::Error::other(format!("no provider for {rel_path:?}")))?
        .parse_file_raw(path, content)
        .map_err(io::Error::other)
}

/// rkyv-serialise a graph's v5 fragment file. Shared by the re-parse route
/// (`parse_to_fragment`) and the pre-parsed route (`write_fragment_from_graph`)
/// so both emit byte-identical fragment archives.
fn archive_fragments(graph: &LocalGraph) -> io::Result<Vec<u8>> {
    let file = fragment_file_from_local_graph(graph);
    rkyv::to_bytes::<rkyv::rancor::Error>(&file)
        .map(|v| v.into_vec())
        .map_err(io::Error::other)
}

/// Pre-parsed twin of `write_fragment_file`: the dirty file was already parsed
/// upstream (`reanalyze_files`), so this skips the tree-sitter re-parse and
/// derives `fragment_id` from the graph's own `content_hash` (the `xxh3_64`
/// LE bytes) and the path's route key, as `write_fragment_file` does.
fn write_fragment_from_graph(
    overlay_dir: &Path,
    batch_seq: u64,
    input_idx: u64,
    graph: &LocalGraph,
    mtime_ns: u64,
    events: &HashMap<String, DirtyPathEvent>,
    worktree: &Path,
) -> io::Result<StagedFragment> {
    // Manifest keys feed uid streams and graph-file matching, which both use
    // forward slashes (the graph builder normalizes `File.path` that way) —
    // a Windows PathBuf would leak backslashes here and silently kill every
    // overlay↔base match.
    let rel_path = graph.file_path.to_string_lossy().replace('\\', "/");
    let content_hash = graph_content_hash(graph, worktree);
    let fragment_id = fragment_id_for(&rel_path, content_hash);
    let content_hash = format!("{content_hash:016x}");

    let tmp = match archive_fragments(graph) {
        Ok(archive_bytes) => Some(stage_archive(
            overlay_dir,
            &fragment_id,
            batch_seq,
            input_idx,
            &archive_bytes,
        )?),
        Err(_) => None,
    };
    let parse_failed = tmp.is_none();

    let event = events
        .get(&rel_path)
        .map_or(DirtyPathEvent::PARSED_SOURCE, |event| event.parsed());
    let entry = DirtyEventEntry {
        change: event.change,
        class: event.class,
        mtime_ns,
        fragment_id: Some(fragment_id.clone()),
        content_hash: Some(content_hash),
        parse_failed,
        dirty_symbols: symbols_from_graph(graph, &rel_path),
        format: ecp_core::session::overlay::FRAGMENT_FORMAT_V7,
    };
    let outcome = FragmentOutcome {
        fragment_id,
        parse_failed,
    };
    Ok(StagedFragment {
        rel_path,
        entry,
        outcome,
        tmp,
    })
}

/// The fragment id: an xxh3 over the file's route key and its content hash.
/// Two paths with one content and one route share a bin; one content under two
/// routes (`deploy.yaml` against `.github/workflows/deploy.yaml`) parses to
/// different graphs, so it gets two. Both writers call this with the xxh3 of
/// the content, so the re-parse and the pre-parsed route stay byte-identical.
fn fragment_id_for(rel_path: &str, content_hash: u64) -> String {
    // A path no rule routes has no parse; it gets the all-ones key, which no
    // routed path can produce (`RouteKey::SNIFFED` is only the language byte).
    let route_key = match FileRouter::PATH_ONLY.route(rel_path) {
        RouteOutcome::Routed(route) => route.key().to_bytes(),
        RouteOutcome::Excluded(_) | RouteOutcome::Unrouted => [u8::MAX; 3],
    };
    let mut keyed = [0u8; 11];
    keyed[..3].copy_from_slice(&route_key);
    keyed[3..].copy_from_slice(&content_hash.to_le_bytes());
    format!("{:016x}", xxh3_64(&keyed))
}

/// A graph's content hash. An omitted file (over the cap, unreadable at parse
/// time) carries the zero hash on purpose, since the parse cache must not trust
/// a hash of content nobody read. The fragment key needs the real content, so
/// read the file from the session's worktree; a file that is gone hashes as
/// empty.
fn graph_content_hash(graph: &LocalGraph, worktree: &Path) -> u64 {
    if graph.content_hash != [0u8; 8] {
        return u64::from_le_bytes(graph.content_hash);
    }
    hash_worktree_file(worktree, &graph.file_path)
}

/// The xxh3 of `rel` under `worktree`, streamed so an over-cap file is never
/// loaded whole; the digest equals `xxh3_64` of the content. A file that is
/// gone or unreadable hashes as empty. An unknown worktree root (an empty
/// path) would resolve `rel` against the process cwd, so it yields the zero
/// hash, which the promotion compare never matches.
fn hash_worktree_file(worktree: &Path, rel: &Path) -> u64 {
    use std::io::Read;
    if worktree.as_os_str().is_empty() {
        return 0;
    }
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    let mut buf = [0u8; 64 * 1024];
    let Ok(mut file) = fs::File::open(worktree.join(rel)) else {
        return xxh3_64(&[]);
    };
    loop {
        match file.read(&mut buf) {
            Ok(0) => return hasher.digest(),
            Ok(n) => {
                hasher.update(&buf[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return xxh3_64(&[]),
        }
    }
}

static WRITE_BATCH_SEQ: AtomicU64 = AtomicU64::new(0);

// One pipeline per distinct provider set, built on first use and shared by
// every overlay write: a path gets the providers the cold index would build.
use crate::reanalyze::pipeline_for_path;

/// `None` for nodes that are not a declaration two sessions can both edit —
/// File, Import, Route, Process, Document, Section, EntryPoint, PathLiteral and
/// the rest of the reference-level tail.
///
/// `Property`, `EnumVariant` and `Variable` ARE kept: a struct field is a real
/// declaration with `field_read` impact edges, so dropping it made a peer
/// editing `Config.token` invisible to a session whose dirty function reads it.
/// (Function-body locals never reach here — the indexer drops them by design.)
///
/// The declaration variants at the end of `NodeKind` (Struct, Enum, Typedef,
/// Namespace, Module, Trait, Impl, Macro, Annotation) were appended after this
/// mapping was first written — the schema is append-only — so they used to fall
/// through the catch-all. A Rust file holding only `pub struct Config` would
/// then produce an empty declaration list and be invisible to `ecp peers`.
fn map_node_kind(k: &NodeKind) -> Option<SymbolKind> {
    match k {
        NodeKind::Function => Some(SymbolKind::Function),
        NodeKind::Method | NodeKind::Constructor => Some(SymbolKind::Method),
        NodeKind::Class | NodeKind::Struct | NodeKind::Impl => Some(SymbolKind::Struct),
        NodeKind::Interface | NodeKind::Trait => Some(SymbolKind::Trait),
        NodeKind::Enum => Some(SymbolKind::Enum),
        NodeKind::Typedef => Some(SymbolKind::Type),
        NodeKind::Module | NodeKind::Namespace => Some(SymbolKind::Module),
        NodeKind::Const | NodeKind::EnumVariant => Some(SymbolKind::Const),
        NodeKind::Property | NodeKind::Variable => Some(SymbolKind::Unknown),
        NodeKind::Macro | NodeKind::Annotation => Some(SymbolKind::Unknown),
        _ => None,
    }
}

/// Stateful overlay writer for a single session directory.
///
/// Tracks the session dir and exposes `append_dirty` which runs the analyzer
/// pipeline on each recorded path, extracting `dirty_symbols` at write-time.
/// The pipeline is cached process-wide via `OnceLock` to avoid re-registering
/// 16 language providers on every call.
pub struct OverlayWriter {
    session_dir: PathBuf,
}

impl OverlayWriter {
    pub fn new(session_dir: &Path) -> Self {
        Self {
            session_dir: session_dir.to_path_buf(),
        }
    }

    /// Record `path` as dirty and run the analyzer to populate `dirty_symbols`.
    ///
    /// `content_hash` and `fragment_id` are stored verbatim — callers compute
    /// them (e.g. SHA-256 prefix of file content). Unsupported extensions or
    /// empty analyzer output → `parse_failed=true`, `dirty_symbols=[]`.
    pub fn append_dirty(
        &mut self,
        path: &Path,
        content_hash: &str,
        fragment_id: &str,
    ) -> io::Result<()> {
        let rel = relativise(&self.session_dir, path);
        let (dirty_symbols, parse_failed) = extract_symbols(path, &rel);

        let manifest_path = layout::dirty_events_path(&self.session_dir);
        let size_cap = session_size_cap(&self.session_dir);
        let mut manifest = DirtyEvents::read(&manifest_path)
            .ok()
            .filter(|m| m.is_current(size_cap))
            .unwrap_or_else(|| DirtyEvents::empty(size_cap));

        let event = DirtyPathEvent::PARSED_SOURCE;
        manifest.entries.insert(
            rel.clone(),
            DirtyEventEntry {
                change: event.change,
                class: event.class,
                mtime_ns: 0,
                fragment_id: Some(fragment_id.to_string()),
                content_hash: Some(content_hash.to_string()),
                parse_failed,
                dirty_symbols,
                format: ecp_core::session::overlay::FRAGMENT_FORMAT_V7,
            },
        );
        DirtyEvents::write_atomic(&manifest_path, &manifest)
    }

    /// Read the current `l1/dirty_events.json` for this session.
    pub fn read_dirty(&self) -> io::Result<DirtyEvents> {
        let manifest_path = layout::dirty_events_path(&self.session_dir);
        if manifest_path.exists() {
            DirtyEvents::read(&manifest_path)
        } else {
            Ok(DirtyEvents::empty(session_size_cap(&self.session_dir)))
        }
    }
}

/// Strip the repo_root prefix from `path` so dirty.json carries paths
/// relative to the registry repo, not absolute filesystem paths.
/// Falls back to the file basename if the path is not under repo_root
/// (e.g., absolute tempfile paths in tests).
fn relativise(session_dir: &Path, path: &Path) -> String {
    if let Some(repo_root) = session_dir.parent().and_then(|p| p.parent()) {
        if let Ok(rel) = path.strip_prefix(repo_root) {
            return rel.to_string_lossy().into_owned();
        }
    }
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Declarations per dirty file. Bounded so one generated or bundled file
/// cannot dominate the manifest, which is deserialized on the agent-blocking
/// query path and rewritten on every save.
///
/// Truncation is visible rather than silent: the entry keeps a final
/// `SymbolKind::Unknown` marker named `…+N more (truncated)`, so a reader that
/// finds no overlap can tell "no overlap" from "we stopped looking".
const MAX_SYMBOLS_PER_ENTRY: usize = 400;

/// The declaration surface a peer session can collide on.
///
/// Load-bearing for `ecp peers`: `peer::concern::classify` reads
/// `dirty_symbols` and nothing else, so an entry written without them is
/// invisible to HARD and SOFT alike — both routes into `l1/dirty_events.json`
/// have to fill it.
///
/// This is the file's declaration list, NOT a change set: the file is
/// re-parsed whole and nothing is compared against the base graph, so it
/// cannot say which of these the session actually edited. `classify` states
/// HARD at that granularity deliberately.
fn symbols_from_graph(graph: &LocalGraph, file_str: &str) -> Vec<SymbolRef> {
    let mut symbols: Vec<SymbolRef> = graph
        .nodes
        .iter()
        .filter_map(|n| {
            let kind = map_node_kind(&n.kind)?;
            Some(SymbolRef {
                name: n.name.clone(),
                kind,
                file: file_str.to_string(),
                line_start: n.span.0 + 1, // tree-sitter 0-based → 1-based
                line_end: n.span.2 + 1,
            })
        })
        .take(MAX_SYMBOLS_PER_ENTRY + 1)
        .collect();
    if symbols.len() > MAX_SYMBOLS_PER_ENTRY {
        let dropped = graph
            .nodes
            .iter()
            .filter(|n| map_node_kind(&n.kind).is_some())
            .count()
            - MAX_SYMBOLS_PER_ENTRY;
        symbols.truncate(MAX_SYMBOLS_PER_ENTRY);
        symbols.push(SymbolRef {
            name: format!("…+{dropped} more (truncated)"),
            kind: SymbolKind::Unknown,
            file: file_str.to_string(),
            line_start: 0,
            line_end: 0,
        });
    }
    symbols
}

/// Run the pipeline on `path` and convert nodes to `SymbolRef`s.
/// Returns `(symbols, parse_failed)` — `parse_failed` is true when the
/// pipeline has no provider for the extension or returns no graph.
fn extract_symbols(path: &Path, file_str: &str) -> (Vec<SymbolRef>, bool) {
    let abs = path.to_path_buf();
    // Route by the repo-relative path, not the basename: path rules (a
    // workflow directory, a template directory) decide the provider.
    let rel = PathBuf::from(file_str.replace('\\', "/"));
    // `path` ends in `rel`, so the ancestor `rel` deep is the root whose
    // `.ecp/config.toml` holds the cap.
    let root = abs
        .ancestors()
        .nth(rel.components().count())
        .unwrap_or(abs.as_path());
    let size_cap = ecp_core::config::resolve_size_cap(root);

    let Some(pipeline) = pipeline_for_path(&rel, size_cap) else {
        return (vec![], true);
    };
    let graphs = pipeline.analyze(vec![(abs, rel)]);
    match graphs.into_iter().next() {
        None => (vec![], true),
        Some(graph) => (symbols_from_graph(&graph, file_str), false),
    }
}

#[cfg(test)]
mod hash_tests {
    use super::*;

    /// Stands for: the streamed digest of an over-buffer file equals the
    /// one-shot `xxh3_64` that `promotion.rs` compares with git's hash.
    #[test]
    fn test_hash_worktree_file_larger_than_buffer_matches_xxh3_64() {
        let dir = tempfile::tempdir().unwrap();
        let content: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(dir.path().join("big.bin"), &content).unwrap();

        let got = hash_worktree_file(dir.path(), Path::new("big.bin"));

        assert_eq!(got, xxh3_64(&content));
    }

    /// Stands for: a file that is gone hashes as empty content, as before.
    #[test]
    fn test_hash_worktree_file_missing_hashes_as_empty() {
        let dir = tempfile::tempdir().unwrap();

        let got = hash_worktree_file(dir.path(), Path::new("gone.rs"));

        assert_eq!(got, xxh3_64(&[]));
    }

    /// Stands for: an unknown worktree root never reads a cwd-relative path.
    /// `Cargo.toml` exists in the test's cwd (the crate root); the hash is
    /// the zero hash, not that file's digest.
    #[test]
    fn test_hash_worktree_file_unknown_root_returns_zero() {
        let got = hash_worktree_file(Path::new(""), Path::new("Cargo.toml"));

        assert_eq!(got, 0);
    }
}

#[cfg(test)]
mod flags_tests {
    use super::*;

    const SOURCE: &[u8] = b"namespace Lib
{
    public class Svc
    {
        private int count;
        public void M() {}
        private static void S() {}
    }
}
";

    fn fragment_symbols() -> Vec<FragmentSymbol> {
        let graph = parse_graph("Lib/Svc.cs", SOURCE, SizeCap::BUILT_IN).unwrap();
        fragment_file_from_local_graph(&graph).symbols
    }

    /// Stands for: a fragment carries a method's real flags, and `Some(0)`
    /// (public instance) stays distinct from the absence of a meta, which is
    /// `None`. The overlay's C# access rule reads them.
    #[test]
    fn test_fragment_file_from_local_graph_method_carries_its_flags() {
        let symbols = fragment_symbols();
        let flags_of = |name: &str| {
            symbols
                .iter()
                .find(|s| s.name == name && s.kind == NodeKind::Method)
                .unwrap_or_else(|| panic!("{name} missing"))
                .flags
        };
        assert_eq!(flags_of("M"), Some(0));
        assert!(flags_of("S").is_some_and(|flags| flags != 0));
    }

    /// Stands for: a symbol the parser pairs with no function meta (a type,
    /// a field) has flags `None`, never `Some(0)`: the overlay treats `None`
    /// as unknown and reports the gap.
    #[test]
    fn test_fragment_file_from_local_graph_non_callable_has_no_flags() {
        let symbols = fragment_symbols();
        let non_callable: Vec<_> = symbols
            .iter()
            .filter(|s| {
                !matches!(
                    s.kind,
                    NodeKind::Function | NodeKind::Method | NodeKind::Constructor
                )
            })
            .collect();
        assert!(
            non_callable.iter().any(|s| s.name == "Svc"),
            "{non_callable:?}"
        );
        assert!(
            non_callable.iter().all(|s| s.flags.is_none()),
            "{non_callable:?}"
        );
    }
}
