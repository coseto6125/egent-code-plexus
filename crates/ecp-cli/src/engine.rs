use crate::session::view_cache;
use ecp_core::contracts::routing::SizeCap;
use ecp_core::graph::{ArchivedZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC};
use ecp_core::session::view::{CallGap, HiddenDecl};
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
    /// Changed paths `git status` printed that the dirty set could not name
    /// (not UTF-8, or not repo-relative): their uncommitted edits are in no
    /// overlay. Set by `ReadContext` from the gate's dirty set.
    unreadable_changes: usize,
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
            view: GraphView::L2Only,
            is_stale_for_sha: false,
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
        let view = self.overlay_view.get().and_then(Option::as_ref);
        crate::output::merge_caveats(self.graph_caveat(), self.dirty_lines(None, view))
    }

    /// [`Engine::caveat`] for a command that takes the caveat before it runs
    /// its query and then reads the overlay view (`find`, `impact --batch`):
    /// it builds the view first.
    pub fn caveat_reading_view(&self) -> Option<String> {
        crate::output::merge_caveats(self.graph_caveat(), self.dirty_set_caveat(None))
    }

    fn graph_caveat(&self) -> Option<String> {
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

    /// The part of [`Engine::caveat`] that the dirty set gives, for callers
    /// that word the graph's staleness themselves (a by-repo `find` names the
    /// stale repo): changed paths the dirty set could not name, then the base
    /// declarations the overlay could not pair. `repo` prefixes each line on a
    /// multi-repo answer. It builds the overlay view when it is not built yet,
    /// because `find` takes the caveat before it runs the query; with no
    /// overlay input that is a single `OnceLock` read.
    pub fn dirty_set_caveat(&self, repo: Option<&str>) -> Option<String> {
        self.dirty_lines(repo, self.overlay_view())
    }

    fn dirty_lines(&self, repo: Option<&str>, view: Option<&OverlayView>) -> Option<String> {
        let label = |line: String| match repo {
            Some(name) => format!("{name}: {line}"),
            None => line,
        };
        let unreadable = self.unreadable_changes_line();
        let ambiguous =
            view.and_then(|view| unpaired_declarations_line(view.hidden_declarations()));
        let gaps = view.and_then(|view| call_gaps_line(view.call_gaps()));
        let stale = view.and_then(|view| stale_closure_line(view.stale_closure()));
        [ambiguous, gaps, stale]
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
    /// built it from the same graph, binary, fragments and tombstones. `None`
    /// when no session overlay is attached, the working tree is clean, or
    /// every entry failed validation — callers then traverse the base graph
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
                let view = OverlayView::build_with_closure(
                    graph,
                    &files,
                    &records,
                    &inputs.tombstones,
                    &inputs.stale,
                )?;
                if let Some(key) = key {
                    view_cache::write(dir, key, &view);
                }
                Some(view)
            })
            .as_ref()
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

/// The caveat line for calls the overlay could not resolve as a full index
/// would ([`CallGap`]), or `None` when there are none. Effect first, then
/// each call with its reason code, then the fix.
fn stale_closure_line(stale: &ecp_core::session::closure::StaleClosure) -> Option<String> {
    if stale.is_empty() {
        return None;
    }
    let why = match (stale.unbounded, stale.not_computed) {
        (Some(n), _) => {
            format!("the edit reaches {n} clean dependent file(s), past the re-resolve limit")
        }
        (None, true) => "no dependency closure was computed for these edits".to_owned(),
        (None, false) => "the overlay does not re-resolve these kinds".to_owned(),
    };
    let names = if stale.names.is_empty() {
        String::new()
    } else {
        format!(" (removed names: {})", stale.names.join(", "))
    };
    Some(format!(
        "results may be incomplete: clean files keep their indexed {} edges{names}, \
         because {why}. Run `ecp admin index --repo .` and rerun.",
        stale.kinds.join(", ")
    ))
}

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
        assert!(unbounded.contains("indexed Calls, Imports edges"));
        assert!(unbounded.contains("reaches 300 clean dependent file(s)"));

        let routes = stale_closure_line(&StaleClosure {
            kinds: kinds(&["EntryPoint", "HandlesRoute"]),
            names: kinds(&["save"]),
            ..StaleClosure::default()
        })
        .unwrap();
        assert!(routes.contains("(removed names: save)"));
        assert!(routes.contains("does not re-resolve these kinds"));
        assert!(routes.contains("`ecp admin index --repo .`"));
        assert!(!routes.contains('\n'), "one reason per line");
    }
}
