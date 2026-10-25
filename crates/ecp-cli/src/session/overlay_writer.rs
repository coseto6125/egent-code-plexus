//! L1 fragment writer: re-parses a dirty source file, archives the per-file
//! graph fragment via rkyv, updates `dirty_files.json` + `session_meta.overlay_version`.
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
use ecp_core::contracts::routing::SizeCap;
use ecp_core::graph::NodeKind;
use ecp_core::registry::{atomic_write_json, replace_file, FileLock};
use ecp_core::session::overlay::{SymbolKind, SymbolRef};
use ecp_core::session::{DirtyEntry, DirtyFiles, SessionMeta};
use rayon::prelude::*;
use rkyv::{Archive, Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
/// (import-scoped) resolution. `DirtyEntry.format == FRAGMENT_FORMAT_V5`
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
    let return_types: std::collections::HashMap<_, _> = graph
        .raw_function_metas
        .iter()
        .filter(|meta| !meta.return_type.is_empty())
        .map(|meta| (meta.span, meta.return_type.clone()))
        .collect();
    FragmentFile {
        symbols: graph
            .nodes
            .iter()
            .map(|n| FragmentSymbol {
                name: n.name.clone(),
                kind: n.kind,
                span: n.span,
                owner_class: n.owner_class.clone(),
                is_exported: n.is_exported,
                calls: n.calls.clone(),
                heritage: n.heritage.clone(),
                variant_refs: n.field_reads.clone(),
                return_type: matches!(
                    n.kind,
                    NodeKind::Function | NodeKind::Method | NodeKind::Constructor
                )
                .then(|| return_types.get(&n.span).cloned())
                .flatten(),
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

    let overlay_dir = session_dir.join("graph_overlay");
    fs::create_dir_all(&overlay_dir)?;
    let batch_seq = WRITE_BATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    // The cap of the worktree this session overlays, resolved once per batch.
    // An unreadable meta fails the commit below; until then the env or
    // built-in cap stands.
    let size_cap = SessionMeta::read(&session_dir.join("session_meta.json"))
        .map(|sm| ecp_core::config::resolve_size_cap(Path::new(&sm.source_worktree)))
        .unwrap_or_else(|_| ecp_core::config::env_size_cap(None));

    // Parallelise fragment writes: hashing and parsing (CPU) + atomic file write (IO).
    // Coalesces the per-file IO while keeping the final manifest commit atomic.
    let results: Vec<io::Result<StagedFragment>> = inputs
        .par_iter()
        .enumerate()
        .map(|(idx, input)| {
            write_fragment_file(&overlay_dir, batch_seq, idx as u64, input, size_cap)
        })
        .collect();

    commit_fragment_results(session_dir, &overlay_dir, results)
}

/// Write fragments from already-parsed `LocalGraph`s — the incremental path's
/// root optimisation: `reanalyze_files` parsed these files once already, so
/// reparsing them here (the old `FragmentInput` route) would pay the tree-sitter
/// cost twice. Each graph carries its own `content_hash` (same `xxh3_64` the
/// re-parse route hashes), so `fragment_id` stays byte-identical; `mtimes` is
/// the paired modification time per graph (stat is cheap, the graph doesn't
/// carry it). Graphs and `mtimes` MUST be equal length and aligned by index.
pub fn write_dirty_fragments_from_graphs(
    session_dir: &Path,
    graphs: &[LocalGraph],
    mtimes: &[u64],
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
    if graphs.is_empty() {
        return Ok(Vec::new());
    }

    let overlay_dir = session_dir.join("graph_overlay");
    fs::create_dir_all(&overlay_dir)?;
    let batch_seq = WRITE_BATCH_SEQ.fetch_add(1, Ordering::Relaxed);

    let results: Vec<io::Result<StagedFragment>> = graphs
        .par_iter()
        .zip(mtimes.par_iter())
        .enumerate()
        .map(|(idx, (graph, &mtime_ns))| {
            write_fragment_from_graph(&overlay_dir, batch_seq, idx as u64, graph, mtime_ns)
        })
        .collect();

    commit_fragment_results(session_dir, &overlay_dir, results)
}

/// The lock a writer holds while it publishes fragment bins and commits the
/// manifest that names them, and `admin gc` holds while it judges a bin
/// orphan and deletes it. A bin's name is its content hash, so without the
/// lock gc could delete a bin a writer had just re-published under the name
/// gc judged orphan.
pub fn overlay_lock_path(session_dir: &Path) -> PathBuf {
    session_dir.join("overlay.lock")
}

/// One dirty file's fragment before publish: its manifest entry and, when
/// the archive was written, the fsynced tmp that becomes `<fragment_id>.bin`.
struct StagedFragment {
    rel_path: String,
    entry: DirtyEntry,
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
/// bins, then commit the manifest + `overlay_version` bump in one atomic
/// write each (coalesces N read-modify-write cycles down to 2).
fn commit_fragment_results(
    session_dir: &Path,
    overlay_dir: &Path,
    results: Vec<io::Result<StagedFragment>>,
) -> io::Result<Vec<FragmentOutcome>> {
    let staged = results.into_iter().collect::<io::Result<Vec<_>>>()?;
    let mut outcomes = Vec::with_capacity(staged.len());
    {
        let _publish = FileLock::acquire_exclusive(&overlay_lock_path(session_dir))?;
        for fragment in &staged {
            if let Some(tmp) = &fragment.tmp {
                let bin = overlay_dir.join(format!("{}.bin", fragment.entry.fragment_id));
                replace_file(tmp, &bin)?;
            }
        }
        let manifest_path = session_dir.join("dirty_files.json");
        let mut df = if manifest_path.exists() {
            DirtyFiles::read(&manifest_path)?
        } else {
            DirtyFiles::empty()
        };
        // Another binary's entries parse by other rules. The freshness gate
        // re-parses every dirty file of such a manifest, so none is kept.
        if !df.from_current_builder() {
            df = DirtyFiles::empty();
        }
        for fragment in staged {
            df.entries.insert(fragment.rel_path, fragment.entry);
            outcomes.push(fragment.outcome);
        }
        atomic_write_json(&manifest_path, &df)?;
    }

    let sm_path = session_dir.join("session_meta.json");
    let mut sm = SessionMeta::read(&sm_path)?;
    sm.overlay_version += outcomes.len() as u32;
    sm.last_touched = chrono::Utc::now().to_rfc3339();
    atomic_write_json(&sm_path, &sm)?;

    Ok(outcomes)
}

fn write_fragment_file(
    overlay_dir: &Path,
    batch_seq: u64,
    input_idx: u64,
    input: &FragmentInput,
    size_cap: SizeCap,
) -> io::Result<StagedFragment> {
    let content_hash = content_hash_hex(&input.content);
    let fragment_id = content_hash.clone();

    // Same forward-slash invariant as `write_fragment_from_graph`.
    let rel_path = input.rel_path.replace('\\', "/");
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

    let entry = DirtyEntry {
        mtime_ns: input.mtime_ns,
        content_hash: content_hash.clone(),
        fragment_id: fragment_id.clone(),
        tantivy_delta_segment: None,
        parse_failed,
        dirty_symbols,
        format: ecp_core::session::overlay::FRAGMENT_FORMAT_V5,
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
/// LE bytes — `u64::from_le_bytes` + `{:016x}` reproduces `xxh3_hex16` exactly).
fn write_fragment_from_graph(
    overlay_dir: &Path,
    batch_seq: u64,
    input_idx: u64,
    graph: &LocalGraph,
    mtime_ns: u64,
) -> io::Result<StagedFragment> {
    let content_hash = format!("{:016x}", u64::from_le_bytes(graph.content_hash));
    let fragment_id = content_hash.clone();

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

    // Manifest keys feed uid streams and graph-file matching, which both use
    // forward slashes (the graph builder normalizes `File.path` that way) —
    // a Windows PathBuf would leak backslashes here and silently kill every
    // overlay↔base match.
    let rel_path = graph.file_path.to_string_lossy().replace('\\', "/");
    let entry = DirtyEntry {
        mtime_ns,
        content_hash: content_hash.clone(),
        fragment_id: fragment_id.clone(),
        tantivy_delta_segment: None,
        parse_failed,
        dirty_symbols: symbols_from_graph(graph, &rel_path),
        format: ecp_core::session::overlay::FRAGMENT_FORMAT_V5,
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

fn content_hash_hex(bytes: &[u8]) -> String {
    crate::repo_identity::xxh3_hex16(bytes)
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

        let manifest_path = self.session_dir.join("dirty_files.json");
        let mut df = if manifest_path.exists() {
            DirtyFiles::read(&manifest_path)?
        } else {
            DirtyFiles::empty()
        };

        df.entries.insert(
            rel.clone(),
            DirtyEntry {
                mtime_ns: 0,
                content_hash: content_hash.to_string(),
                fragment_id: fragment_id.to_string(),
                tantivy_delta_segment: None,
                parse_failed,
                dirty_symbols,
                format: ecp_core::session::overlay::FRAGMENT_FORMAT_V5,
            },
        );
        atomic_write_json(&manifest_path, &df)
    }

    /// Read the current `dirty_files.json` for this session.
    pub fn read_dirty(&self) -> io::Result<DirtyFiles> {
        let manifest_path = self.session_dir.join("dirty_files.json");
        if manifest_path.exists() {
            DirtyFiles::read(&manifest_path)
        } else {
            Ok(DirtyFiles::empty())
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
/// invisible to HARD and SOFT alike — both routes into `dirty_files.json`
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
