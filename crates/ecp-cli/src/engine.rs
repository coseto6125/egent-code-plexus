use ecp_core::graph::{ArchivedZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC};
use ecp_core::session::OverlayView;
use memmap2::Mmap;
use rkyv::rancor::Error;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

pub mod test_counters {
    use std::sync::atomic::AtomicUsize;

    /// Incremented on every full `rkyv::access` walk over a graph file.
    /// Integration tests assert that `header_compatible` followed by
    /// `Engine::load` on the same file walks it once, not twice.
    pub static DEEP_VALIDATION_COUNT: AtomicUsize = AtomicUsize::new(0);
}

/// Identity of a graph file whose full structural validation already passed
/// in this process. `ensure_index` validates through `header_compatible`, and
/// `Engine::load` validated the same bytes again a few microseconds later:
/// on an 88 MB graph that second walk cost 2-4 ms of every command.
type ValidatedKey = (PathBuf, u64, Option<SystemTime>);

static VALIDATED: OnceLock<Mutex<HashSet<ValidatedKey>>> = OnceLock::new();

fn validated_set() -> &'static Mutex<HashSet<ValidatedKey>> {
    VALIDATED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn validated_key(graph_path: &Path, file: &File) -> Option<ValidatedKey> {
    let meta = file.metadata().ok()?;
    Some((graph_path.to_path_buf(), meta.len(), meta.modified().ok()))
}

/// Full structural validation, skipped when this process already validated
/// a file of the same path, length and mtime. A rebuilt graph lands under a
/// new commit dir, so an identical key means identical bytes.
fn validate_once(graph_path: &Path, file: &File, bytes: &[u8]) -> io::Result<()> {
    let key = validated_key(graph_path, file);
    if let Some(key) = &key {
        if validated_set().lock().is_ok_and(|set| set.contains(key)) {
            return Ok(());
        }
    }
    validate_header(bytes)?;
    if let (Some(key), Ok(mut set)) = (key, validated_set().lock()) {
        set.insert(key);
    }
    Ok(())
}

pub struct Engine {
    mmap: Mmap,
    graph_path: PathBuf,
    // Phase 3 reserves the slot; Phase 5 will wire L1 overlay merge into query paths.
    #[allow(dead_code)]
    overlay_dir: Option<PathBuf>,
    /// Worktree root the overlay session belongs to — needed to validate
    /// fragment mtimes when materialising the [`OverlayView`].
    worktree_root: Option<PathBuf>,
    /// Lazily built query-time overlay merge view. `None` inside the cell =
    /// clean tree (or no session): traversals take their original branch.
    overlay_view: OnceLock<Option<OverlayView>>,
    view: GraphView,
    /// True when this engine was loaded from a sibling SHA's graph because the
    /// current HEAD had no published graph yet (OOB branch-switch warm-attach
    /// path). LLM consumers should treat results as slightly stale; a background
    /// rebuild for the new SHA is already in flight.
    pub is_stale_for_sha: bool,
    /// True when the loaded graph's commit dir names a SHA behind the repo's
    /// current HEAD. By-repo loaders (`auto_ensure::load_ensured`) resolve the
    /// latest *published* graph, which lags committed-but-unindexed work; the
    /// L1 overlay absorbs the delta for freshness bookkeeping, but queries
    /// read L2 only, so results genuinely miss the newer commits.
    pub behind_head: bool,
}

/// Discriminated view over the L2 graph plus an optional L1 overlay.
/// `L2Only` is the PureReference fast-path: callers can guarantee no
/// `graph_overlay/` access (spec invariant F5). `L2WithOverlay` signals
/// that the session has dirty fragments; the overlay merge implementation
/// itself is deferred to P2 of the index-layout follow-up tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphView {
    L2Only,
    L2WithOverlay,
}

impl Engine {
    pub fn load<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        // Canonicalize so callers (especially `index_dir()`) always see an
        // absolute path. The legacy default `.ecp/graph.bin` arrives here
        // relative when `graph_path::resolve` falls through (e.g. cwd is
        // outside any registered repo) — without canonicalize, `index_dir()`
        // would yield `.ecp` and the tantivy lookup would resolve against
        // whatever the process cwd happens to be at search time.
        let graph_path =
            fs::canonicalize(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_path_buf());
        let file = File::open(&graph_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        // Queries touch most of the graph (node slice scans, CSR walks), so a
        // cold first invocation pays one page fault per 4K page. WillNeed lets
        // the kernel read ahead in bulk; best-effort, and a no-op when warm.
        #[cfg(unix)]
        let _ = mmap.advise(memmap2::Advice::WillNeed);
        validate_once(&graph_path, &file, &mmap)?;
        Ok(Self {
            mmap,
            graph_path,
            overlay_dir: None,
            worktree_root: None,
            overlay_view: OnceLock::new(),
            view: GraphView::L2Only,
            is_stale_for_sha: false,
            behind_head: false,
        })
    }

    /// Load from a sibling SHA's `graph.bin` for the warm-attach fast path.
    /// Sets `is_stale_for_sha = true` so the caller can surface a staleness
    /// note to the LLM consumer without altering query results.
    pub fn load_warm<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let mut eng = Self::load(path)?;
        eng.is_stale_for_sha = true;
        Ok(eng)
    }

    /// The single caveat string for query output, or `None` when results are
    /// trustworthy. Sources today: warm-attach staleness and behind-HEAD
    /// graphs; future caveats (blind-spot coverage, ambiguity-suppressed
    /// edges) extend the chain. Routed into the payload's `result` field by
    /// `output::emit_with_caveat`, so a `found:false` under a stale graph is
    /// no longer indistinguishable from a definitive "does not exist".
    pub fn caveat(&self) -> Option<String> {
        if self.is_stale_for_sha {
            Some(
                "results may be incomplete: graph is a warm-attach from a sibling commit \
                 (current HEAD not yet indexed); symbols added since are invisible. A background \
                 rebuild is in flight — rerun, or `ecp admin index --force --repo .` for a \
                 definitive answer."
                    .to_string(),
            )
        } else if self.behind_head {
            Some(
                "results may be incomplete: graph predates the repo's current HEAD, so \
                 committed changes since are not indexed. Run `ecp admin index --repo <path>` \
                 for a definitive answer."
                    .to_string(),
            )
        } else {
            None
        }
    }

    /// SessionState-driven constructor (spec §5.1). Classifies the session and
    /// picks the right load path: PureReference → L2-only view (no overlay
    /// touch, satisfies invariant F5); AugmentedReference → L2 + record the
    /// overlay dir so the (P2) merge layer can find it; Stale → error so the
    /// caller falls back to a fresh session.
    ///
    /// Exercised by `tests/engine_session_state_test.rs`; bin paths still
    /// reach the graph via `Engine::load`. Will become reachable from bin
    /// once the P5 session-aware query path lands.
    #[allow(dead_code)]
    pub fn open(repo_root: &Path, sid: &str) -> io::Result<Self> {
        let state = crate::session::state::classify(repo_root, sid);
        match state {
            ecp_core::session::SessionState::PureReference { l2_dirname, .. } => {
                let l2_dir = repo_root.join("commits").join(&l2_dirname);
                let mut eng = Self::load(l2_dir.join("graph.bin"))?;
                eng.view = GraphView::L2Only;
                Ok(eng)
            }
            ecp_core::session::SessionState::AugmentedReference { l2_dirname, .. } => {
                let l2_dir = repo_root.join("commits").join(&l2_dirname);
                let overlay_dir = repo_root.join("sessions").join(sid);
                let mut eng = Self::load(l2_dir.join("graph.bin"))?;
                eng.overlay_dir = Some(overlay_dir);
                eng.view = GraphView::L2WithOverlay;
                Ok(eng)
            }

            ecp_core::session::SessionState::Stale { reason } => Err(io::Error::other(format!(
                "session stale: {reason:?}; remove via `ecp admin sessions reset <id>`"
            ))),
        }
    }

    /// Attach an L1 session overlay dir (`~/.ecp/<repo>/sessions/<sid>/`) so
    /// query paths can surface dirty graph fragments over the L2 base. Wired
    /// from `main.rs` after the engine loads (when a session overlay resolves).
    /// `worktree_root` anchors fragment mtime validation for `overlay_view`.
    pub fn with_overlay(mut self, dir: PathBuf, worktree_root: PathBuf) -> Self {
        self.overlay_dir = Some(dir);
        self.worktree_root = Some(worktree_root);
        self.view = GraphView::L2WithOverlay;
        self
    }

    /// The query-time overlay merge view, built once per process on first
    /// use. `None` when no session overlay is attached, the working tree is
    /// clean, or every fragment failed validation — callers then traverse
    /// the base graph exactly as before (zero added cost).
    pub fn overlay_view(&self) -> Option<&OverlayView> {
        self.overlay_view
            .get_or_init(|| {
                let dir = self.overlay_dir.as_deref()?;
                let root = self.worktree_root.as_deref()?;
                let graph = self.graph().ok()?;
                let inputs = crate::session::overlay_reader::load_view_inputs(dir, root).ok()?;
                OverlayView::build(graph, &inputs)
            })
            .as_ref()
    }

    /// Current view discriminator. PureReference sessions yield `L2Only`;
    /// AugmentedReference and back-compat `load` callers yield `L2WithOverlay`.
    ///
    /// Asserted by `tests/engine_session_state_test.rs` to verify the
    /// `Engine::open` view-selection invariant; becomes a bin-level concern
    /// once the P5 session-aware merge layer reads it on every query.
    #[allow(dead_code)]
    pub fn view(&self) -> GraphView {
        self.view
    }

    pub fn graph(&self) -> Result<&ArchivedZeroCopyGraph, Error> {
        // Every constructor (`load` / `load_warm` / `open`) runs the validated
        // `rkyv::access` in `validate_header` before the engine exists, and the
        // mmap is read-only + atomic-renamed, so its bytes never change while
        // the engine is alive. Re-validating on each `graph()` call (26 hot-path
        // sites, O(graph size) structural walk each) is therefore redundant;
        // `access_unchecked` skips it.
        //
        // SAFETY: `self.mmap` passed `validate_header` (full `rkyv::access`
        // structural validation) at construction time and is an immutable,
        // never-reopened read-only mapping, so the archive layout invariants
        // `access_unchecked` assumes still hold.
        Ok(unsafe { rkyv::access_unchecked::<ArchivedZeroCopyGraph>(&self.mmap) })
    }

    /// Resolved L2 commit directory: `graph.bin` lives directly inside
    /// `~/.ecp/<repo>/commits/<dirname>/`, so the index dir is the immediate
    /// parent of the graph path. Tantivy and meta.json also live there.
    pub fn index_dir(&self) -> Option<&Path> {
        self.graph_path.parent()
    }

    /// Resolved L1 session overlay dir, set via `with_overlay`. None when
    /// no session is attached (e.g. queries without `--session-id`).
    pub fn overlay_dir(&self) -> Option<&Path> {
        self.overlay_dir.as_deref()
    }
}

/// Cheap predicate for `auto_ensure`: returns `true` iff `graph.bin`
/// can be memory-mapped and passes magic + version validation. Any
/// I/O / mmap / rkyv access / magic / version failure returns `false`
/// so the caller treats a schema break the same as a stale graph and
/// triggers a clean rebuild — without surfacing `InvalidData` on a
/// CLI upgrade that bumped `GRAPH_FORMAT_VERSION`.
pub fn header_compatible(graph_path: &Path) -> bool {
    // Same canonicalization as `Engine::load`, or the validated-set keys never
    // match and every command walks the file twice again.
    let graph_path = fs::canonicalize(graph_path).unwrap_or_else(|_| graph_path.to_path_buf());
    let Ok(file) = File::open(&graph_path) else {
        return false;
    };
    let Ok(mmap) = (unsafe { Mmap::map(&file) }) else {
        return false;
    };
    validate_once(&graph_path, &file, &mmap).is_ok()
}

/// Reject `graph.bin` files that don't carry the ecp magic header or
/// whose on-disk format version this reader doesn't understand. Both
/// failure modes would otherwise be undetected by `rkyv::access`
/// (which only validates structural layout, not field values) and
/// surface as segfaults or silent misinterpretation downstream.
fn validate_header(bytes: &[u8]) -> io::Result<()> {
    test_counters::DEEP_VALIDATION_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let archived = rkyv::access::<ArchivedZeroCopyGraph, Error>(bytes).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("graph.bin: structural validation failed: {e}"),
        )
    })?;
    if archived.magic != GRAPH_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "graph.bin: bad magic — expected {:?}, got {:?}",
                GRAPH_MAGIC, archived.magic
            ),
        ));
    }
    let version = archived.version.to_native();
    if version != GRAPH_FORMAT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "graph.bin: incompatible format version {version} \
                 (this reader expects {GRAPH_FORMAT_VERSION}) — run `ecp analyze` to regenerate"
            ),
        ));
    }
    Ok(())
}
