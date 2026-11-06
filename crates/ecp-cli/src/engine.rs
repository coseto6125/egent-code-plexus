use crate::session::sources::RecordedSources;
use crate::session::view_cache;
use ecp_core::contracts::routing::SizeCap;
use ecp_core::graph::{ArchivedZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC};
use ecp_core::session::closure::StaleClosure;
use ecp_core::session::view::{CallGap, HiddenDecl};
use ecp_core::session::OverlayView;
use memmap2::Mmap;
use rkyv::rancor::Error;
use rustc_hash::{FxHashMap, FxHashSet};
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
/// a file of the same path, length and mtime (`key`, from
/// [`validated_key`]). A rebuilt graph lands under a new commit dir, so an
/// identical key means identical bytes.
fn validate_once(key: Option<&ValidatedKey>, bytes: &[u8]) -> io::Result<()> {
    if let Some(key) = key {
        if validated_set().lock().is_ok_and(|set| set.contains(key)) {
            return Ok(());
        }
    }
    validate_header(bytes)?;
    if let (Some(key), Ok(mut set)) = (key, validated_set().lock()) {
        set.insert(key.clone());
    }
    Ok(())
}

pub struct Engine {
    mmap: Mmap,
    graph_path: PathBuf,
    /// Path, length and mtime of the mapped graph, stat'ed from the open
    /// handle: the view cache keys on the bytes this engine reads, not on
    /// whatever file the path names later.
    graph_stamp: Option<ValidatedKey>,
    // Phase 3 reserves the slot; Phase 5 will wire L1 overlay merge into query paths.
    #[allow(dead_code)]
    overlay_dir: Option<PathBuf>,
    /// Worktree root the overlay session belongs to — needed to validate
    /// fragment mtimes when materialising the [`OverlayView`].
    worktree_root: Option<PathBuf>,
    /// The cap this query's L1 refresh stamped, set via `with_overlay`.
    overlay_size_cap: Option<SizeCap>,
    /// Lazily built query-time overlay merge view. `None` inside the cell =
    /// clean tree (or no session): traversals take their original branch.
    overlay_view: OnceLock<Option<OverlayView>>,
    /// Each accepted fragment's path → the content hash it parsed, set by
    /// `overlay_view` before it can return, whatever it returns.
    overlay_hashes: OnceLock<FxHashMap<String, u64>>,
    /// Each accepted tombstone's path, set beside `overlay_hashes`.
    overlay_tombstones: OnceLock<FxHashSet<String>>,
    view: GraphView,
    /// True when this engine was loaded from a sibling SHA's graph because the
    /// current HEAD had no published graph yet (OOB branch-switch warm-attach
    /// path). LLM consumers should treat results as slightly stale; a background
    /// rebuild for the new SHA is already in flight.
    pub is_stale_for_sha: bool,
    /// Whether the overlay brought a warm-attached graph to HEAD. `None`
    /// until the read context checks it; a warm engine with `None` is
    /// unverified.
    head_coverage: Option<HeadCoverage>,
    /// True when the loaded graph's commit dir names a SHA behind the repo's
    /// current HEAD. By-repo loaders (`auto_ensure::load_ensured`) resolve the
    /// latest *published* graph, which lags committed-but-unindexed work; the
    /// L1 overlay absorbs the delta for freshness bookkeeping, but queries
    /// read L2 only, so results genuinely miss the newer commits.
    pub behind_head: bool,
    /// Changed paths `git status` printed that the dirty set could not name
    /// (not UTF-8, or not repo-relative): their uncommitted edits are in no
    /// overlay. Set by `ReadContext` from the gate's dirty set.
    unreadable_changes: usize,
}

/// The command an unverified answer names as its next step.
pub const REINDEX_COMMAND: &str = "ecp admin index --force --repo .";

/// Whether the overlay re-parsed every file that differs between a
/// warm-attached sibling graph and the worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadCoverage {
    /// The refresh wrote a fragment or a tombstone for every changed source
    /// path in `sources`. Answers are HEAD's only while the view this query
    /// reads still holds each of them: a concurrent query may retire the
    /// entries between the refresh and the view build.
    Covered { sources: Vec<String> },
    /// The overlay could not cover the changes, for this reason.
    Uncovered(String),
}

/// Why a covered warm attach is unverified after all.
const COVER_NOT_IN_VIEW: &str =
    "the overlay view lacks a changed file the refresh covered (another \
     query retired its entry, or the file changed since the refresh)";

/// Which overlay lines a caveat keeps. The clean-file-edges line stays in
/// every scope: it speaks for clean files, and any row may be one.
#[derive(Clone, Copy)]
enum LineScope<'s> {
    /// Every line: the commands that do not scope their caveat.
    All,
    /// A line that lists dirty files stays when it names a file of the
    /// payload.
    Files(&'s FxHashSet<&'s str>),
    /// An impact answer on `target`: as `Files`, and the unpaired-declarations
    /// line always, because a hidden declaration also hides the edges the
    /// traversal would have followed, so its file is in no row. A C# call
    /// line stays when a call names `target`: the caller it may lack an edge
    /// from is in no row either.
    Impact {
        files: &'s FxHashSet<&'s str>,
        target: &'s str,
    },
}

impl LineScope<'_> {
    fn keeps_hidden(self, hidden: &[HiddenDecl]) -> bool {
        match self {
            Self::All | Self::Impact { .. } => true,
            Self::Files(files) => hidden.iter().any(|d| files.contains(d.path.as_str())),
        }
    }

    fn keeps_gaps(self, gaps: &[CallGap]) -> bool {
        match self {
            Self::All => true,
            Self::Files(files) => gaps.iter().any(|g| files.contains(g.path.as_str())),
            Self::Impact { files, target } => gaps
                .iter()
                .any(|g| g.member == target || files.contains(g.path.as_str())),
        }
    }
}

/// Discriminated view over the L2 graph plus an optional L1 overlay.
/// `L2Only` is the PureReference fast-path: callers can guarantee no
/// `l1/` overlay access (spec invariant F5). `L2WithOverlay` signals
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
        let graph_stamp = validated_key(&graph_path, &file);
        validate_once(graph_stamp.as_ref(), &mmap)?;
        Ok(Self {
            mmap,
            graph_path,
            graph_stamp,
            overlay_dir: None,
            worktree_root: None,
            overlay_size_cap: None,
            overlay_view: OnceLock::new(),
            overlay_hashes: OnceLock::new(),
            overlay_tombstones: OnceLock::new(),
            view: GraphView::L2Only,
            is_stale_for_sha: false,
            head_coverage: None,
            behind_head: false,
            unreadable_changes: 0,
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

    /// The single caveat string for query output, one reason per line, or
    /// `None` when results are trustworthy: warm-attach staleness or a
    /// behind-HEAD graph, then the dirty-set lines of [`Engine::dirty_set_caveat`].
    /// Routed into the payload's `result` field by `output::emit_with_caveat`,
    /// so a `found:false` under a stale graph is no longer indistinguishable
    /// from a definitive "does not exist".
    ///
    /// The unpaired-declarations line comes only from a view this query
    /// already built: a command that answers from the base graph alone
    /// (`routes`) hides nothing, and building the view for it is wasted work.
    /// A command that takes the caveat before it reads the view calls
    /// [`Engine::caveat_reading_view`].
    pub fn caveat(&self) -> Option<String> {
        // The staleness check of a covered warm attach builds the view, so it
        // runs first and the lines read the same view.
        let graph = self.graph_caveat();
        let view = self.overlay_view.get().and_then(Option::as_ref);
        crate::output::merge_caveats(graph, self.dirty_lines(None, view, LineScope::All))
    }

    /// [`Engine::caveat`] scoped to an answer whose payload names
    /// `result_files`: a line that lists dirty files (unpaired declarations,
    /// unresolved C# calls) stays only when it names one of them. The
    /// staleness lines, the unreadable-paths line and the clean-file-edges
    /// line stay whatever the files. It builds the overlay view when it is
    /// not built yet.
    pub fn caveat_for<'a>(
        &self,
        result_files: impl IntoIterator<Item = &'a str>,
    ) -> Option<String> {
        crate::output::merge_caveats(
            self.graph_caveat(),
            self.dirty_set_caveat_for(None, result_files),
        )
    }

    /// [`Engine::caveat_for`] for an impact answer on the declaration named
    /// `target`: the unpaired-declarations line stays whatever the files,
    /// and a C# call line stays when a call names `target`.
    pub fn impact_caveat_for<'a>(
        &self,
        target: &str,
        result_files: impl IntoIterator<Item = &'a str>,
    ) -> Option<String> {
        let files: FxHashSet<&str> = result_files.into_iter().collect();
        let graph = self.graph_caveat();
        let scope = LineScope::Impact {
            files: &files,
            target,
        };
        crate::output::merge_caveats(graph, self.dirty_lines(None, self.overlay_view(), scope))
    }

    /// [`Engine::dirty_set_caveat`] scoped to `result_files` as
    /// [`Engine::caveat_for`] scopes it.
    pub fn dirty_set_caveat_for<'a>(
        &self,
        repo: Option<&str>,
        result_files: impl IntoIterator<Item = &'a str>,
    ) -> Option<String> {
        let files: FxHashSet<&str> = result_files.into_iter().collect();
        self.dirty_lines(repo, self.overlay_view(), LineScope::Files(&files))
    }

    /// [`Engine::caveat`] for a command that takes the caveat before it runs
    /// its query and then reads the overlay view (`find`, `impact --batch`):
    /// it builds the view first.
    pub fn caveat_reading_view(&self) -> Option<String> {
        crate::output::merge_caveats(self.graph_caveat(), self.dirty_set_caveat(None))
    }

    pub(crate) fn set_head_coverage(&mut self, coverage: HeadCoverage) {
        self.head_coverage = Some(coverage);
    }

    /// Why this engine's answers may lack what HEAD holds: a warm attach the
    /// overlay did not bring to HEAD, or whose view lost a path the refresh
    /// covered. `None` for every other engine. A covered warm attach builds
    /// the overlay view here, so the verdict speaks for the view the answer
    /// reads.
    pub fn unverified_reason(&self) -> Option<&str> {
        if !self.is_stale_for_sha {
            return None;
        }
        match &self.head_coverage {
            Some(HeadCoverage::Covered { sources }) => sources
                .iter()
                .any(|rel| !self.overlay_holds(rel))
                .then_some(COVER_NOT_IN_VIEW),
            Some(HeadCoverage::Uncovered(reason)) => Some(reason.as_str()),
            None => Some("no check compared the sibling commit with HEAD"),
        }
    }

    /// The sentence a miss on this engine appends: why the answer may lack
    /// what HEAD holds, and the command that settles it.
    pub fn unverified_note(&self) -> Option<String> {
        self.unverified_reason().map(|reason| {
            format!(
                "This miss is unverified: the graph is a warm-attach from a sibling commit \
                 and {reason}. Run `{REINDEX_COMMAND}` for a definitive answer"
            )
        })
    }

    fn graph_caveat(&self) -> Option<String> {
        if let Some(reason) = self.unverified_reason() {
            let rebuild = if crate::background::background_rebuild_skipped() {
                "No background rebuild runs (ECP_SKIP_BG_REBUILD is set) — run"
            } else {
                "A background rebuild is in flight — rerun, or"
            };
            Some(format!(
                "results may be incomplete: graph is a warm-attach from a sibling commit \
                 (current HEAD not yet indexed) and {reason}; symbols added since are \
                 invisible. {rebuild} `{REINDEX_COMMAND}` for a definitive answer."
            ))
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

    /// The part of [`Engine::caveat`] that the dirty set gives, for callers
    /// that word the graph's staleness themselves (a by-repo `find` names the
    /// stale repo): changed paths the dirty set could not name, then the base
    /// declarations the overlay could not pair. `repo` prefixes each line on a
    /// multi-repo answer. It builds the overlay view when it is not built yet,
    /// because `find` takes the caveat before it runs the query; with no
    /// overlay input that is a single `OnceLock` read.
    pub fn dirty_set_caveat(&self, repo: Option<&str>) -> Option<String> {
        self.dirty_lines(repo, self.overlay_view(), LineScope::All)
    }

    fn dirty_lines(
        &self,
        repo: Option<&str>,
        view: Option<&OverlayView>,
        scope: LineScope<'_>,
    ) -> Option<String> {
        let label = |line: String| match repo {
            Some(name) => format!("{name}: {line}"),
            None => line,
        };
        let unreadable = self.unreadable_changes_line();
        let overlay = view.map_or([None, None, None], |view| {
            overlay_lines(
                view.hidden_declarations(),
                view.call_gaps(),
                view.stale_closure(),
                scope,
            )
        });
        overlay
            .into_iter()
            .fold(unreadable.map(label), |acc, line| {
                crate::output::merge_caveats(acc, line.map(label))
            })
    }

    fn unreadable_changes_line(&self) -> Option<String> {
        let n = self.unreadable_changes;
        (n > 0).then(|| {
            format!(
                "{n} changed path(s) are not UTF-8 or not repo-relative, so their \
                 uncommitted edits are not in this answer"
            )
        })
    }

    pub(crate) fn set_unreadable_changes(&mut self, n: usize) {
        self.unreadable_changes = n;
    }

    /// SessionState-driven constructor. Classifies the session and
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

    /// The worktree top an attached overlay belongs to. `None` without an
    /// overlay.
    pub fn worktree_root(&self) -> Option<&Path> {
        self.worktree_root.as_deref()
    }

    /// Attach an L1 session overlay dir (`~/.ecp/<repo>/sessions/<sid>/`) so
    /// query paths can surface dirty graph fragments over the L2 base. Wired
    /// from `read_context` after the engine loads (when a session overlay
    /// resolves). `worktree_root` anchors fragment mtime validation for
    /// `overlay_view`; it must be the worktree top the fragment paths are
    /// relative to.
    /// `size_cap` is the cap this query's L1 refresh stamped, when one ran;
    /// otherwise the reader resolves the effective cap itself.
    pub fn with_overlay(
        mut self,
        dir: PathBuf,
        worktree_root: PathBuf,
        size_cap: Option<SizeCap>,
    ) -> Self {
        self.overlay_dir = Some(dir);
        self.overlay_size_cap = size_cap;
        self.worktree_root = Some(worktree_root);
        self.view = GraphView::L2WithOverlay;
        self
    }

    /// The query-time overlay merge view, built once per process on first
    /// use, or decoded from the session's view cache when an earlier process
    /// built it from the same graph, binary, fragments (with the content
    /// hash each one parsed) and tombstones. A view whose source reads found
    /// text other than the recorded one is not stored. `None` when no
    /// session overlay is attached, the working tree is clean, or every
    /// entry failed validation — callers then traverse the base graph
    /// exactly as before (zero added cost).
    pub fn overlay_view(&self) -> Option<&OverlayView> {
        self.overlay_view
            .get_or_init(|| {
                let dir = self.overlay_dir.as_deref()?;
                let root = self.worktree_root.as_deref()?;
                let graph = self.graph().ok()?;
                // The refresh's cap when one ran this query, so the reader
                // accepts what the refresh just wrote.
                let size_cap = self
                    .overlay_size_cap
                    .unwrap_or_else(|| ecp_core::config::resolve_size_cap(root));
                let inputs =
                    crate::session::overlay_reader::view_inputs_under(dir, root, size_cap).ok()?;
                let _ = self.overlay_hashes.set(
                    inputs
                        .fragments
                        .iter()
                        .filter_map(|f| Some((f.rel_path.clone(), f.content_hash?)))
                        .collect(),
                );
                let _ = self
                    .overlay_tombstones
                    .set(inputs.tombstones.iter().cloned().collect());
                if inputs.is_empty() {
                    return None;
                }
                let key = self.view_cache_key(inputs.hash);
                let nodes = graph.nodes.len();
                if let Some(view) = key.and_then(|key| view_cache::read(dir, key, nodes)) {
                    return Some(view);
                }
                let (files, records) =
                    crate::session::overlay_reader::decode_view_inputs_with_records(
                        &inputs.fragments,
                    );
                // The view reads only text whose hash the key covers (the
                // graph's or an accepted fragment's), so a view whose reads
                // all found that text is a function of the key. One that
                // found other text or no file at a recorded hash is not
                // stored.
                let sources = RecordedSources::new(root, graph, &inputs);
                let view = OverlayView::build_with_configs(
                    graph,
                    &files,
                    &records,
                    &inputs.tombstones,
                    &inputs.stale,
                    &inputs.configs,
                    Some(&|rel: &str| sources.read(rel)),
                )?;
                if let Some(key) = key.filter(|_| sources.all_recorded()) {
                    view_cache::write(dir, key, &view);
                }
                Some(view)
            })
            .as_ref()
    }

    /// Whether `root/rel` holds the text this engine's answers for `rel` came
    /// from: the accepted fragment's text when the overlay view supplies the
    /// path, else the text the graph indexed. A file that is gone, changed,
    /// unreadable or recorded with no hash is not recorded, and neither is a
    /// path that is no regular file or is over the parse cap: the hook runs
    /// this on every search, so it never blocks on a FIFO or reads a file
    /// whole.
    pub fn file_text_is_recorded(&self, root: &Path, rel: &str) -> bool {
        self.overlay_view();
        let dirty = self.overlay_hashes.get_or_init(FxHashMap::default);
        self.text_matches(root, rel, dirty)
    }

    /// Whether the overlay view supplies `rel`: only then can
    /// [`Self::file_text_is_recorded`] and
    /// [`Self::file_text_is_base_recorded`] differ.
    pub fn overlay_supplies(&self, rel: &str) -> bool {
        self.overlay_view();
        self.overlay_hashes
            .get()
            .is_some_and(|hashes| hashes.contains_key(rel))
    }

    /// Whether the overlay view holds `rel` as an accepted fragment or an
    /// accepted tombstone.
    fn overlay_holds(&self, rel: &str) -> bool {
        self.overlay_supplies(rel)
            || self
                .overlay_tombstones
                .get()
                .is_some_and(|tombstones| tombstones.contains(rel))
    }

    /// As [`Self::file_text_is_recorded`], against the base graph's hash
    /// only: the proof for a fact read from the base graph's edges, which an
    /// overlay refresh does not rebuild.
    pub fn file_text_is_base_recorded(&self, root: &Path, rel: &str) -> bool {
        self.text_matches(root, rel, &FxHashMap::<String, u64>::default())
    }

    fn text_matches<K>(&self, root: &Path, rel: &str, dirty: &FxHashMap<K, u64>) -> bool
    where
        K: std::borrow::Borrow<str> + std::hash::Hash + Eq,
    {
        let Ok(graph) = self.graph() else {
            return false;
        };
        crate::session::sources::recorded_hash(graph, dirty, rel).is_some_and(|hash| {
            crate::session::sources::text_is_recorded(root, rel, hash, || {
                self.overlay_size_cap
                    .unwrap_or_else(|| ecp_core::config::resolve_size_cap(root))
            })
        })
    }

    /// The key `overlay_view` stores and looks up its view under, for the
    /// inputs hash `view_inputs` returns. `None` when the mapped
    /// file's stamp is unknown; the view is then built without the cache.
    pub fn view_cache_key(&self, inputs_hash: u64) -> Option<u64> {
        let (path, len, mtime) = self.graph_stamp.as_ref()?;
        let nodes = self.graph().ok()?.nodes.len();
        view_cache::key((path.as_path(), *len, (*mtime)?), nodes, inputs_hash)
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
    validate_once(validated_key(&graph_path, &file).as_ref(), &mmap).is_ok()
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
                 (this reader expects {GRAPH_FORMAT_VERSION}) — run `ecp admin index --force` to regenerate"
            ),
        ));
    }
    if !archived.has_module_facts() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "graph.bin: import / export facts missing or inconsistent — run `ecp admin index --force` \
             to regenerate",
        ));
    }
    Ok(())
}

/// The overlay's caveat lines that `scope` keeps: unpaired declarations,
/// unresolved C# calls, clean-file edges.
fn overlay_lines(
    hidden: &[HiddenDecl],
    gaps: &[CallGap],
    stale: &StaleClosure,
    scope: LineScope<'_>,
) -> [Option<String>; 3] {
    [
        scope
            .keeps_hidden(hidden)
            .then(|| unpaired_declarations_line(hidden))
            .flatten(),
        scope
            .keeps_gaps(gaps)
            .then(|| call_gaps_line(gaps))
            .flatten(),
        stale_closure_line(stale),
    ]
}

/// Most groups the unpaired-declarations line names; the rest fold into a count.
const UNPAIRED_GROUPS_SHOWN: usize = 5;

/// The caveat line for base declarations the overlay hid for lack of an
/// identity to pair them by, or `None` when it hid none. Effect first, then
/// the groups, then what the hiding does not mean. The counts are hidden
/// declarations, never callers: a hidden declaration may have none.
fn unpaired_declarations_line(hidden: &[HiddenDecl]) -> Option<String> {
    if hidden.is_empty() {
        return None;
    }
    let total: u32 = hidden.iter().map(|d| d.count).sum();
    let mut groups: Vec<String> = hidden
        .iter()
        .take(UNPAIRED_GROUPS_SHOWN)
        .map(|d| {
            let owner = if d.owner.is_empty() {
                String::new()
            } else {
                format!("{}.", d.owner)
            };
            format!(
                "{} {owner}{} x{} ({})",
                d.kind.as_str(),
                d.name,
                d.count,
                d.path
            )
        })
        .collect();
    let shown: u32 = hidden
        .iter()
        .take(UNPAIRED_GROUPS_SHOWN)
        .map(|d| d.count)
        .sum();
    if total > shown {
        groups.push(format!("and {} more", total - shown));
    }
    Some(format!(
        "results may be incomplete: the dirty overlay could not pair {total} base \
         declaration(s) by identity, so they and their edges are hidden: {}. This does \
         not mean they or their callers were deleted; an empty result is no proof of no \
         callers. Run `ecp admin index --repo .` and rerun.",
        groups.join(", ")
    ))
}

/// The caveat line for the clean-file edges and nodes the invalidation
/// closure left as indexed, or `None` when it left none.
fn stale_closure_line(stale: &StaleClosure) -> Option<String> {
    if stale.is_empty() {
        return None;
    }
    let why = match (stale.unbounded, stale.not_computed) {
        (Some(n), _) => {
            format!("the edit reaches {n} clean dependent file(s), past the re-resolve limit")
        }
        (None, true) => "no dependency closure was computed for these edits".to_owned(),
        (None, false) if !stale.configs.is_empty() => {
            "the overlay could not apply the new reading of these configs (the parser \
             rejects one, or one is a Cargo.toml) and resolves with the indexed one"
                .to_owned()
        }
        (None, false) => "the overlay does not re-resolve these kinds".to_owned(),
    };
    let names = if stale.names.is_empty() {
        String::new()
    } else {
        format!(" (removed names: {})", stale.names.join(", "))
    };
    let configs = if stale.configs.is_empty() {
        String::new()
    } else {
        format!(" (kept configs: {})", stale.configs.join(", "))
    };
    Some(format!(
        "results may be incomplete: clean files keep their indexed {} edges and nodes{names}{configs}, \
         because {why}. Run `ecp admin index --repo .` and rerun.",
        stale.kinds.join(", ")
    ))
}

/// The caveat line for calls the overlay could not resolve as a full index
/// would ([`CallGap`]), or `None` when there are none. Effect first, then
/// each call with its reason code, then the fix.
fn call_gaps_line(gaps: &[CallGap]) -> Option<String> {
    if gaps.is_empty() {
        return None;
    }
    let mut listed: Vec<String> = gaps
        .iter()
        .take(UNPAIRED_GROUPS_SHOWN)
        .map(|gap| {
            format!(
                "{} -> {} ({}:{}, {})",
                gap.caller, gap.member, gap.path, gap.line, gap.reason
            )
        })
        .collect();
    if gaps.len() > UNPAIRED_GROUPS_SHOWN {
        listed.push(format!("and {} more", gaps.len() - UNPAIRED_GROUPS_SHOWN));
    }
    Some(format!(
        "results may be incomplete: the dirty overlay cannot resolve {} C# call(s) as \
         a full index would: {}. A listed call may lack an edge, or bind a stale \
         or wrong target. Run `ecp admin index --repo .` and rerun.",
        gaps.len(),
        listed.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::graph::NodeKind;

    fn hidden(owner: &str, name: &str, path: &str, count: u32) -> HiddenDecl {
        HiddenDecl {
            path: path.to_string(),
            kind: NodeKind::Method,
            owner: owner.to_string(),
            name: name.to_string(),
            count,
        }
    }

    /// Stands for: the line names the hidden declarations and their count in
    /// the effect-first shape consumers parse; an empty list gives no line.
    #[test]
    fn test_unpaired_declarations_line_one_group_names_declaration_and_count() {
        assert_eq!(unpaired_declarations_line(&[]), None);

        let line = unpaired_declarations_line(&[hidden("K", "save", "src/K.java", 2)]).unwrap();

        assert!(line.starts_with(
            "results may be incomplete: the dirty overlay could not pair 2 base declaration(s)"
        ));
        assert!(line.contains("hidden: Method K.save x2 (src/K.java). This does not mean"));
        assert!(line.contains("`ecp admin index --repo .`"));
        assert!(!line.contains('\n'), "one reason per line");
    }

    /// Stands for: at most 5 groups are named, and "and N more" counts the
    /// hidden declarations of the rest, not the groups.
    #[test]
    fn test_unpaired_declarations_line_over_five_groups_counts_declarations_in_more() {
        let hidden: Vec<HiddenDecl> = (0..7)
            .map(|i| hidden("", &format!("f{i}"), "a.cpp", if i >= 5 { 3 } else { 2 }))
            .collect();

        let line = unpaired_declarations_line(&hidden).unwrap();

        assert!(line.contains("could not pair 16 base declaration(s)"));
        assert!(line.contains("Method f4 x2 (a.cpp), and 6 more."));
        assert!(!line.contains("f5"), "the sixth group folds into the count");
    }

    fn gap(caller: &str, line: u32, reason: &str) -> CallGap {
        CallGap {
            path: "src/App.cs".to_string(),
            line,
            caller: caller.to_string(),
            member: "Spin".to_string(),
            reason: reason.to_string(),
        }
    }

    /// Stands for: the line names each call site, its member and its
    /// machine-readable reason code, and no gap gives no line.
    #[test]
    fn test_call_gaps_line_names_call_site_and_reason() {
        assert_eq!(call_gaps_line(&[]), None);

        let line = call_gaps_line(&[gap("Use", 5, "cs-extension-stale-edge")]).unwrap();

        assert!(line.starts_with(
            "results may be incomplete: the dirty overlay cannot resolve 1 C# call(s)"
        ));
        assert!(line.contains("Use -> Spin (src/App.cs:5, cs-extension-stale-edge)."));
        assert!(line.contains("`ecp admin index --repo .`"));
        assert!(!line.contains('\n'), "one reason per line");
    }

    /// Stands for: at most 5 calls are named; "and N more" counts the rest.
    #[test]
    fn test_call_gaps_line_over_five_calls_counts_rest() {
        let gaps: Vec<CallGap> = (0..7)
            .map(|i| gap(&format!("f{i}"), i, "cs-extension-dirty-declaration"))
            .collect();

        let line = call_gaps_line(&gaps).unwrap();

        assert!(line.contains("cannot resolve 7 C# call(s)"));
        assert!(
            line.contains("f4 -> Spin (src/App.cs:4, cs-extension-dirty-declaration), and 2 more.")
        );
        assert!(!line.contains("f5"), "the sixth call folds into the count");
    }

    fn closure_with(configs: &[&str]) -> StaleClosure {
        StaleClosure {
            unbounded: Some(300),
            kinds: vec!["Calls".to_string()],
            configs: configs.iter().map(|c| c.to_string()).collect(),
            ..StaleClosure::default()
        }
    }

    /// Stands for: a line that lists dirty files outside the payload is
    /// dropped; the same line naming a payload file is kept.
    #[test]
    fn test_overlay_lines_files_scope_outside_files_drops_inside_keeps() {
        let decls = [hidden("K", "save", "src/K.java", 2)];
        let gaps = [gap("Use", 5, "cs-extension-stale-edge")];

        let outside: FxHashSet<&str> = ["src/Other.java"].into_iter().collect();
        let [ambiguous, call_gaps, _] = overlay_lines(
            &decls,
            &gaps,
            &StaleClosure::default(),
            LineScope::Files(&outside),
        );
        assert_eq!((ambiguous, call_gaps), (None, None));

        let inside: FxHashSet<&str> = ["src/K.java", "src/App.cs"].into_iter().collect();
        let [ambiguous, call_gaps, _] = overlay_lines(
            &decls,
            &gaps,
            &StaleClosure::default(),
            LineScope::Files(&inside),
        );
        assert!(ambiguous.is_some_and(|l| l.contains("Method K.save x2")));
        assert!(call_gaps.is_some_and(|l| l.contains("Use -> Spin")));
    }

    /// Stands for: a clean row's caller count can come from the edges the
    /// closure left as indexed, so the clean-file-edges line stays though no
    /// payload file is the overlay's or a kept config. Failed before: the
    /// line needed a supplied file or a kept config.
    #[test]
    fn test_overlay_lines_files_scope_clean_rows_keep_closure_line() {
        let clean: FxHashSet<&str> = ["src/c.ts"].into_iter().collect();
        let [_, _, closure] = overlay_lines(&[], &[], &closure_with(&[]), LineScope::Files(&clean));
        assert!(closure.is_some_and(|l| l.contains("indexed Calls edges")));

        let [_, _, by_config] = overlay_lines(
            &[],
            &[],
            &closure_with(&["tsconfig.json"]),
            LineScope::Files(&clean),
        );
        assert!(by_config.is_some_and(|l| l.contains("kept configs: tsconfig.json")));
    }

    /// Stands for: impact downstream through an edge to a declaration the
    /// overlay hid: the hidden file is in no row, and the unpaired line
    /// stays. Failed before: the line needed the hidden file among the rows.
    #[test]
    fn test_overlay_lines_impact_scope_hidden_file_outside_rows_keeps_unpaired_line() {
        let decls = [hidden("K", "save", "src/K.java", 2)];
        let rows: FxHashSet<&str> = ["src/Caller.java"].into_iter().collect();
        let [ambiguous, _, _] = overlay_lines(
            &decls,
            &[],
            &StaleClosure::default(),
            LineScope::Impact {
                files: &rows,
                target: "run",
            },
        );
        assert!(ambiguous.is_some_and(|l| l.contains("Method K.save x2")));
    }

    /// Stands for: a C# call to the impact target in a file outside the rows
    /// names a caller the answer may lack, so its line stays; a call to
    /// another member outside the rows is dropped. Failed before: the line
    /// needed the call's file among the rows.
    #[test]
    fn test_overlay_lines_impact_scope_gap_member_is_target_keeps_call_line() {
        let gaps = [gap("Use", 5, "cs-partial-bare-call")];
        let rows: FxHashSet<&str> = ["src/Spin.cs"].into_iter().collect();
        let scope = |target| LineScope::Impact {
            files: &rows,
            target,
        };

        let [_, kept, _] = overlay_lines(&[], &gaps, &StaleClosure::default(), scope("Spin"));
        assert!(kept.is_some_and(|l| l.contains("Use -> Spin (src/App.cs:5")));

        let [_, dropped, _] = overlay_lines(&[], &gaps, &StaleClosure::default(), scope("Other"));
        assert_eq!(dropped, None);
    }

    /// Stands for: `caveat()`'s unscoped lines are the lines the line
    /// builders give.
    #[test]
    fn test_overlay_lines_all_scope_matches_unscoped_lines() {
        let decls = [hidden("K", "save", "src/K.java", 2)];
        let gaps = [gap("Use", 5, "cs-extension-stale-edge")];
        let stale = closure_with(&["go.mod"]);
        assert_eq!(
            overlay_lines(&decls, &gaps, &stale, LineScope::All),
            [
                unpaired_declarations_line(&decls),
                call_gaps_line(&gaps),
                stale_closure_line(&stale),
            ]
        );
        assert_eq!(
            overlay_lines(&[], &[], &StaleClosure::default(), LineScope::All),
            [None, None, None]
        );
    }

    fn warm_engine(dir: &Path) -> Engine {
        let mut fx = ecp_core::graph_fixture::GraphFixture::new();
        fx.func("lib.rs", "alpha");
        let path = dir.join("graph.bin");
        fs::write(&path, fx.into_bytes()).unwrap();
        Engine::load_warm(&path).unwrap()
    }

    /// Stands for: an uncovered warm attach gives a miss the unverified note
    /// with its reason and the reindex command; a covered one gives none.
    #[test]
    fn test_unverified_note_uncovered_warm_attach_names_reason_and_command() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = warm_engine(dir.path());

        let unchecked = engine.unverified_note().unwrap();
        assert!(unchecked.contains("unverified"), "{unchecked}");

        engine.set_head_coverage(HeadCoverage::Uncovered("`git diff` failed".into()));
        let note = engine.unverified_note().unwrap();
        assert!(note.contains("unverified") && note.contains("`git diff` failed"));
        assert!(note.contains(REINDEX_COMMAND), "{note}");

        engine.set_head_coverage(HeadCoverage::Covered { sources: vec![] });
        assert_eq!(engine.unverified_reason(), None);
        assert_eq!(engine.unverified_note(), None);
        assert!(
            engine.caveat().is_none(),
            "a covered warm attach drops its line"
        );
    }

    /// Stands for: the refresh covered `gone.rs` with a tombstone, then a
    /// concurrent query that saw HEAD's graph Ready retired the entry before
    /// this query built its view. The warm attach is then unverified, which
    /// `find` answers as `found: null` on a miss. The control builds its view
    /// before the retire and stays covered. Failed before: `Covered` was
    /// trusted whatever the view held.
    #[test]
    fn test_unverified_reason_cover_retired_before_view_build_is_unverified() {
        use crate::session::overlay_writer::{retire_entries, write_dirty_events};
        use ecp_core::contracts::dirty_event::PathClass;
        use ecp_core::session::{layout, DirtyEvents};

        let dir = tempfile::tempdir().unwrap();
        let (worktree, session) = (dir.path().join("wt"), dir.path().join("session"));
        let (kept_dir, raced_dir) = (dir.path().join("kept"), dir.path().join("raced"));
        for d in [&worktree, &session, &kept_dir, &raced_dir] {
            fs::create_dir_all(d).unwrap();
        }
        let cap = SizeCap::BUILT_IN;
        let meta = ecp_core::session::SessionMeta {
            version: 1,
            session_id: "sid".into(),
            pid: None,
            started_at: "2026-11-07T00:00:00Z".into(),
            last_touched: "2026-11-07T00:00:00Z".into(),
            base_sha: "0".repeat(40),
            source_worktree: worktree.to_string_lossy().into_owned(),
            overlay_version: 0,
            watcher_pid: None,
            last_drained_offset: 0,
            agent_name: None,
        };
        ecp_core::session::SessionMeta::write_atomic(&session.join("session_meta.json"), &meta)
            .unwrap();
        DirtyEvents::write_atomic(
            &layout::dirty_events_path(&session),
            &DirtyEvents::empty(cap),
        )
        .unwrap();
        let tombstone = [("gone.rs".to_string(), PathClass::SOURCE)];
        write_dirty_events(&session, cap, &[], &[], &Default::default(), &tombstone).unwrap();
        let covered = || HeadCoverage::Covered {
            sources: vec!["gone.rs".to_string()],
        };
        let attach = |graph_dir: &Path| {
            let mut engine =
                warm_engine(graph_dir).with_overlay(session.clone(), worktree.clone(), Some(cap));
            engine.set_head_coverage(covered());
            engine
        };

        let kept = attach(&kept_dir);
        assert_eq!(
            kept.unverified_reason(),
            None,
            "the view holds the tombstone"
        );

        let raced = attach(&raced_dir);
        let manifest = DirtyEvents::read(&layout::dirty_events_path(&session)).unwrap();
        retire_entries(&session, &manifest.entries.into_iter().collect()).unwrap();
        assert_eq!(raced.unverified_reason(), Some(COVER_NOT_IN_VIEW));
        assert!(raced.unverified_note().is_some());
        assert_eq!(
            kept.unverified_reason(),
            None,
            "a built view keeps its verdict"
        );
    }

    /// Stands for: the line names the kinds clean files keep as indexed and
    /// why (unbounded closure with its count, closure not computed, or a kind
    /// the overlay never re-resolves), and an empty closure gives no line.
    #[test]
    fn test_stale_closure_line_names_kinds_and_cause() {
        use ecp_core::session::closure::StaleClosure;
        let kinds = |k: &[&str]| k.iter().map(|k| k.to_string()).collect::<Vec<_>>();
        assert_eq!(stale_closure_line(&StaleClosure::default()), None);

        let not_computed = stale_closure_line(&StaleClosure::not_computed()).unwrap();
        assert!(not_computed.contains("no dependency closure was computed"));
        assert!(not_computed.contains("Calls"));

        let unbounded = stale_closure_line(&StaleClosure {
            unbounded: Some(300),
            kinds: kinds(&["Calls", "Imports"]),
            ..StaleClosure::default()
        })
        .unwrap();
        assert!(unbounded.contains("indexed Calls, Imports edges and nodes"));
        assert!(unbounded.contains("reaches 300 clean dependent file(s)"));

        let routes = stale_closure_line(&StaleClosure {
            kinds: kinds(&["EntryPoint", "HandlesRoute"]),
            names: kinds(&["save"]),
            ..StaleClosure::default()
        })
        .unwrap();
        assert!(
            routes.contains("indexed EntryPoint, HandlesRoute edges and nodes"),
            "a node kind is not an edge: the line claims both"
        );
        assert!(routes.contains("(removed names: save)"));
        assert!(routes.contains("does not re-resolve these kinds"));
        assert!(routes.contains("`ecp admin index --repo .`"));
        assert!(!routes.contains('\n'), "one reason per line");

        let kept = stale_closure_line(&StaleClosure {
            kinds: kinds(&["Calls", "Imports"]),
            configs: kinds(&["tsconfig.json"]),
            ..StaleClosure::default()
        })
        .unwrap();
        assert!(kept.contains("(kept configs: tsconfig.json)"));
        assert!(kept.contains("could not apply the new reading of these configs"));
        assert!(!kept.contains('\n'), "one reason per line");
    }
}
