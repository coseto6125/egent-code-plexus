use cgn_core::graph::{ArchivedZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC};
use memmap2::Mmap;
use rkyv::rancor::Error;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

pub struct Engine {
    mmap: Mmap,
    graph_path: PathBuf,
    // Phase 3 reserves the slot; Phase 5 will wire L1 overlay merge into query paths.
    #[allow(dead_code)]
    overlay_dir: Option<PathBuf>,
    view: GraphView,
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
        // absolute path. The legacy default `.gnx/graph.bin` arrives here
        // relative when `graph_path::resolve` falls through (e.g. cwd is
        // outside any registered repo) — without canonicalize, `index_dir()`
        // would yield `.gnx` and the tantivy lookup would resolve against
        // whatever the process cwd happens to be at search time.
        let graph_path =
            fs::canonicalize(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_path_buf());
        let file = File::open(&graph_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        validate_header(&mmap)?;
        Ok(Self {
            mmap,
            graph_path,
            overlay_dir: None,
            view: GraphView::L2Only,
        })
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
            cgn_core::session::SessionState::PureReference { l2_dirname, .. } => {
                let l2_dir = repo_root.join("commits").join(&l2_dirname);
                let mut eng = Self::load(l2_dir.join("graph.bin"))?;
                eng.view = GraphView::L2Only;
                Ok(eng)
            }
            cgn_core::session::SessionState::AugmentedReference {
                l2_dirname, ..
            } => {
                let l2_dir = repo_root.join("commits").join(&l2_dirname);
                let overlay_dir = repo_root.join("sessions").join(sid);
                let mut eng = Self::load(l2_dir.join("graph.bin"))?;
                eng.overlay_dir = Some(overlay_dir);
                eng.view = GraphView::L2WithOverlay;
                Ok(eng)
            }
            cgn_core::session::SessionState::Stale { reason } => Err(io::Error::other(
                format!("session stale: {reason:?}; remove via `cgn admin sessions reset <id>`"),
            )),
        }
    }

    /// Attach an L1 session overlay dir (`~/.gnx/<repo>/sessions/<sid>/`)
    /// to merge dirty graph fragments + tantivy delta over the L2 base.
    /// Phase 3 lands the slot; Phase 5 wires the merge logic into query paths.
    #[allow(dead_code)]
    pub fn with_overlay(mut self, dir: PathBuf) -> Self {
        self.overlay_dir = Some(dir);
        self.view = GraphView::L2WithOverlay;
        self
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
        rkyv::access::<ArchivedZeroCopyGraph, Error>(&self.mmap)
    }

    /// Resolved L2 commit directory: `graph.bin` lives directly inside
    /// `~/.gnx/<repo>/commits/<dirname>/`, so the index dir is the immediate
    /// parent of the graph path. Tantivy and meta.json also live there.
    pub fn index_dir(&self) -> Option<&Path> {
        self.graph_path.parent()
    }

    /// Resolved L1 session overlay dir, set via `with_overlay`. None when
    /// no session is attached (e.g. queries without `--session-id`).
    #[allow(dead_code)]
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
    let Ok(file) = File::open(graph_path) else {
        return false;
    };
    let Ok(mmap) = (unsafe { Mmap::map(&file) }) else {
        return false;
    };
    validate_header(&mmap).is_ok()
}

/// Reject `graph.bin` files that don't carry the cgn magic header or
/// whose on-disk format version this reader doesn't understand. Both
/// failure modes would otherwise be undetected by `rkyv::access`
/// (which only validates structural layout, not field values) and
/// surface as segfaults or silent misinterpretation downstream.
fn validate_header(bytes: &[u8]) -> io::Result<()> {
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
                 (this reader expects {GRAPH_FORMAT_VERSION}) — run `cgn analyze` to regenerate"
            ),
        ));
    }
    Ok(())
}
